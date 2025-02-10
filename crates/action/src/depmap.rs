use anyhow::Context as _;
use cealn_action_context::{ConcreteDepmapResolution, Context};
use cealn_data::{
    action::{ActionOutput, BuildDepmap, BuildDepmapEntry},
    depmap::ConcreteFiletreeType,
    file_entry::{FileEntry, FileEntryRef, FileHash},
};
use cealn_depset::{ConcreteFiletree, DepMap};
use compio_core::buffer::AllowCopy;
use ring::digest::SHA256;

pub async fn build<C: Context>(
    context: &C,
    action: &BuildDepmap<ConcreteFiletreeType>,
) -> anyhow::Result<ActionOutput> {
    let mut builder = ConcreteFiletree::builder();
    for (mount_path, directive) in &action.entries {
        match &directive {
            BuildDepmapEntry::Reference(concrete_reference) => match &concrete_reference.subpath {
                Some(sub_path) => match context.lookup_concrete_depmap(concrete_reference).await? {
                    ConcreteDepmapResolution::Depmap(sub_depmap) => {
                        todo!()
                    }
                    ConcreteDepmapResolution::Subpath(sub_depmap, sub_sub_path) => {
                        // FIXME: not 100% sure order is right
                        if sub_path == &sub_sub_path {
                            builder.merge_filtered(mount_path.as_ref(), sub_path.as_ref(), &[".*"], sub_depmap);
                        } else {
                            todo!()
                        }
                    }
                },
                None => match context.lookup_concrete_depmap(concrete_reference).await? {
                    ConcreteDepmapResolution::Depmap(sub_depmap) => {
                        builder.merge(mount_path.as_ref(), sub_depmap);
                    }
                    ConcreteDepmapResolution::Subpath(sub_depmap, sub_path) => {
                        builder.merge_filtered(mount_path.as_ref(), sub_path.as_ref(), &[".*"], sub_depmap);
                    }
                },
            },
            BuildDepmapEntry::Directory => {
                builder.insert(mount_path.as_ref(), FileEntryRef::Directory);
            }
            BuildDepmapEntry::File { content, executable } => {
                let mut cachefile = context.tempfile("build-depmap-file-literal", *executable).await?;
                let digest = ring::digest::digest(&SHA256, content.as_bytes());
                let digest = FileHash::Sha256(digest.as_ref().try_into().unwrap());
                cachefile
                    .ensure_open()
                    .await?
                    .write_all(AllowCopy(content.as_bytes()))
                    .await?;
                context
                    .move_to_cache_prehashed(cachefile, digest.as_ref(), *executable)
                    .await?;
                builder.insert(
                    mount_path.as_ref(),
                    FileEntryRef::Regular {
                        content_hash: digest.as_ref(),
                        executable: *executable,
                    },
                );
            }
            BuildDepmapEntry::Symlink { target } => {
                builder.insert(mount_path.as_ref(), FileEntryRef::Symlink(target));
            }
            BuildDepmapEntry::Filter { base, prefix, patterns } => {
                let prefix = prefix.normalize_require_descending().context("prefix escaped root")?;
                match context.lookup_concrete_depmap(base).await? {
                    ConcreteDepmapResolution::Depmap(sub_depmap) => {
                        builder.merge_filtered(mount_path.as_ref(), prefix.as_ref(), patterns, sub_depmap);
                    }
                    ConcreteDepmapResolution::Subpath(sub_depmap, sub_path) => {
                        builder.merge_filtered(
                            mount_path.as_ref(),
                            sub_path.join(prefix.as_ref()).as_ref(),
                            patterns,
                            sub_depmap,
                        );
                    }
                }
            }
        }
    }
    let files = context.register_concrete_filetree_depmap(builder.build()).await?;
    Ok(ActionOutput {
        files,
        stdout: None,
        stderr: None,
    })
}
