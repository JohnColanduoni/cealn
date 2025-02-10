use std::io::Write;

use anyhow::{bail, Context as AnyhowContext};
use bumpalo::Bump;
use cealn_depset::{depmap, ConcreteFiletree, DepMap};
use futures::{channel::mpsc, StreamExt};
use git2::{build::RepoBuilder, ObjectType, Oid, TreeEntry};

use cealn_action_context::Context;
use cealn_data::{
    action::{ActionOutput, GitClone},
    file_entry::{FileEntry, FileEntryRef, FileHash},
    label::{LabelPath, LabelPathBuf, NormalizedDescending},
};

#[tracing::instrument("git::clone", level = "debug", err, skip(context))]
pub async fn clone<C: Context>(context: &C, action: &GitClone) -> anyhow::Result<ActionOutput> {
    let _events = context.events().fork();

    let (entry_tx, mut entry_rx) = mpsc::unbounded();

    let clone_dest = context.tempdir("git-clone").await?;

    let cloner = compio_executor::spawn_blocking_handle({
        let url = action.url.clone();
        let revision = action.revision.clone();
        async move {
            let mut builder = RepoBuilder::new();
            builder.bare(true);
            let repo = builder.clone(&url, clone_dest.path())?;

            let commit = repo
                .find_commit(Oid::from_str(&revision).with_context(|| format!("invalid revision {:?}", revision))?)?;
            let tree = commit.tree()?;

            let arena = Bump::new();

            let mut tree_entries: Vec<RootedTreeEntry> = tree
                .iter()
                .map(|entry| RootedTreeEntry {
                    entry,
                    parent_path: None,
                })
                .collect();
            while let Some(entry) = tree_entries.pop() {
                let Some(name) = entry.entry.name().and_then(|x| LabelPath::new(x).ok()) else {
            // Ignore non-utf8 filenames
            continue
        };
                let path = if let Some(parent_path) = entry.parent_path.as_deref() {
                    parent_path.join(name)
                } else {
                    name.to_owned()
                };
                let path = path.normalize_require_descending().unwrap().into_owned();
                let sendable_entry = match entry.entry.kind() {
                    Some(ObjectType::Tree) => {
                        let object = arena.alloc(entry.entry.to_object(&repo)?);
                        let subtree = object.as_tree().context("expected tree")?;
                        tree_entries.extend(subtree.iter().map(|child_entry| RootedTreeEntry {
                            entry: child_entry,
                            parent_path: Some(path.clone()),
                        }));
                        UnprocessedFileEntry::Directory { path }
                    }
                    Some(ObjectType::Blob) => {
                        let object = entry.entry.to_object(&repo)?;
                        let blob = object.as_blob().context("expected blob")?;
                        let blob_data = blob.content();
                        let executable = entry.entry.filemode() & 0o100 != 0;

                        // TODO: for raw blobs, pass blob path so we can read ourselves in another thread
                        // FIXME: symlinks?
                        UnprocessedFileEntry::FileContent {
                            path,
                            content: blob_data.to_owned(),
                            executable,
                        }
                    }
                    _ => continue,
                };

                if let Err(_) = entry_tx.unbounded_send(sendable_entry) {
                    bail!("entry stream dropped");
                }
            }

            Ok(())
        }
    });

    let mut depmap = ConcreteFiletree::builder();

    while let Some(entry) = entry_rx.next().await {
        match entry {
            UnprocessedFileEntry::FileContent {
                path,
                content,
                executable,
            } => {
                let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
                let mut cachefile = context.tempfile("git-blob", executable).await?;
                let file_output = cachefile.ensure_open().await?;
                hasher.update(&content);
                file_output.write_all(content).await?;
                let file_digest = hasher.finish();
                let content_hash = FileHash::Sha256(file_digest.as_ref().try_into().unwrap());
                context
                    .move_to_cache_prehashed(cachefile, content_hash.as_ref(), executable)
                    .await?;

                // FIXME: symlinks?
                let map_entry = FileEntry::Regular {
                    content_hash,
                    executable,
                };
                depmap.insert(path.as_ref(), map_entry.as_ref());
            }
            UnprocessedFileEntry::Directory { path } => {
                depmap.insert(path.as_ref(), FileEntryRef::Directory);
            }
        }
    }

    let depmap = depmap.build();
    let depmap = context.register_concrete_filetree_depmap(depmap).await?;

    cloner.await?;

    Ok(ActionOutput {
        files: depmap,
        stdout: None,
        stderr: None,
    })
}

struct RootedTreeEntry<'tree> {
    entry: TreeEntry<'tree>,
    parent_path: Option<NormalizedDescending<LabelPathBuf>>,
}

enum UnprocessedFileEntry {
    FileContent {
        path: NormalizedDescending<LabelPathBuf>,
        content: Vec<u8>,
        executable: bool,
    },
    Directory {
        path: NormalizedDescending<LabelPathBuf>,
    },
}
