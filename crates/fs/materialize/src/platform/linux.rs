use std::{
    collections::BTreeMap,
    io, mem,
    path::{Path, PathBuf},
    sync::Arc,
};

use anyhow::{bail, Context as AnyhowContext};

use cealn_data::{
    depmap::{ConcreteFiletreeType, DepmapHash, DepmapType},
    file_entry::{FileEntry, FileEntryRef},
    label::{LabelPath, NormalizedDescending},
};
use cealn_depset::{ConcreteFiletree, DepMap};
use compio_core::buffer::AllowTake;
use compio_fs::{Directory, File};
use dashmap::DashMap;
use futures::{future::RemoteHandle, prelude::*, stream::FuturesUnordered};
use serde::{Deserialize, Serialize};
use tracing::{debug_span, Instrument, Span};

use crate::MaterializeContext;

pub struct MaterializeCache {
    shared: Arc<Shared>,
}

struct Shared {
    cache_dir: PathBuf,
    context: Arc<dyn MaterializeContext>,
    building_depmaps: DashMap<DepmapHash, Arc<futures::lock::Mutex<()>>>,
}

pub struct Materialized {
    direct_path: PathBuf,
    overlays: Vec<Overlay>,
    depmap_hash: DepmapHash,
}

pub struct Overlay {
    pub dest_subpath: String,
    pub src_subpath: String,
    pub materialized: Materialized,
}

#[derive(Serialize, Deserialize, Debug)]
struct Stamp {
    overlays: Vec<StampOverlay>,
}

#[derive(Serialize, Deserialize, Debug)]
struct StampOverlay {
    pub dest_subpath: String,
    pub src_subpath: String,
    pub depmap_hash: DepmapHash,
}

// TODO: don't pull this out of our ass
const OVERLAY_THRESHOLD: usize = 256;

// TODO: don't pull this out of our ass
const DIRECT_BRANCH_SIZE: usize = 256;
// TODO: don't pull this out of our ass
const MAX_CONCURRENT_BRANCHES: usize = 32;

impl MaterializeCache {
    pub fn new(cache_dir: PathBuf, context: Arc<dyn MaterializeContext>) -> MaterializeCache {
        let shared = Arc::new(Shared {
            cache_dir,
            context,
            building_depmaps: Default::default(),
        });
        MaterializeCache { shared }
    }

    #[tracing::instrument(level = "info", err, skip(self, depmap), fields(depmap.hash = ?depmap.hash()))]
    pub async fn materialize<'a>(&'a self, depmap: ConcreteFiletree) -> anyhow::Result<Materialized> {
        self.shared.clone().materialize_internal(depmap).await
    }
}

pub async fn materialize_for_output<C>(
    context: &C,
    output_path: &PathBuf,
    depmap: ConcreteFiletree,
) -> anyhow::Result<()>
where
    C: MaterializeContext,
{
    Directory::create_all(&output_path).await?;
    let mut build_destination = Directory::open(&output_path).await?;

    // FIXME: parallel
    // FIXME: order
    let mut reversed: Vec<_> = depmap.iter().collect::<Result<Vec<_>, _>>()?;
    reversed.reverse();
    'entries: for (k, v) in reversed {
        let mut retry = false;
        'retry: loop {
            let result = match v {
                FileEntryRef::Regular {
                    content_hash,
                    executable,
                } => {
                    let file_guard = context
                        .lookup_file(content_hash, executable)
                        .await?
                        .with_context(|| format!("failed to find cache entry for file {:?} -> {:?}", k, v))?;
                    build_destination.link_at(k.as_ref(), &*file_guard).await
                }
                FileEntryRef::Symlink(target) => build_destination.symlink_at(k.as_ref(), target).await,
                FileEntryRef::Directory => {
                    // FIXME: does this make sense?
                    if k.as_str().is_empty() {
                        continue 'entries;
                    }
                    build_destination.create_at_directory(k.as_ref()).await
                }
            };
            match result {
                Ok(()) => continue 'entries,
                Err(ref err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    // This is fine, a partial build may have happened here previously
                    continue 'entries;
                }
                Err(ref err) if err.kind() == io::ErrorKind::NotFound && !retry => {
                    // This is most likely caused by a parent directory not existing
                    if let Some(parent) = k.parent() {
                        build_destination.create_at_directory_all(parent).await?;
                        retry = true;
                        continue 'retry;
                    } else {
                        bail!("root materialize directory not found for {:?} -> {:?}", k, v);
                    }
                }
                Err(err) => return Err(anyhow::Error::from(err).context(format!("while building entry at {:?}", k))),
            }
        }
    }

    Ok(())
}

impl Materialized {
    #[inline]
    pub fn direct_path(&self) -> &Path {
        &self.direct_path
    }

    #[inline]
    pub fn overlays(&self) -> &[Overlay] {
        &self.overlays
    }
}

impl Shared {
    #[tracing::instrument(level = "debug", err, skip(self, depmap), fields(depmap.hash = ?depmap.hash()))]
    async fn materialize_internal(self: Arc<Self>, depmap: ConcreteFiletree) -> anyhow::Result<Materialized> {
        loop {
            let destination_path = match depmap.hash() {
                DepmapHash::Sha256(digest) => {
                    let hex_digest = hex::encode(&digest);
                    let mut destination_path = self.cache_dir.clone();
                    destination_path.push("sha256");
                    destination_path.push(&hex_digest[..2]);
                    destination_path.push(&hex_digest);
                    destination_path
                }
            };

            let stamp_path = destination_path.with_extension("stamp");
            match File::open(&stamp_path).await {
                Ok(mut stamp_file) => {
                    let mut stamp_file_contents = Vec::new();
                    stamp_file.read_to_end(AllowTake(&mut stamp_file_contents)).await?;
                    let stamp: Stamp = serde_json::from_slice(&stamp_file_contents)
                        .with_context(|| format!("failed to parse stamp file {:?}", stamp_path))?;
                    let mut overlays = Vec::new();
                    for overlay in stamp.overlays {
                        let depmap = self
                            .context
                            .lookup_filetree_cache(&overlay.depmap_hash)
                            .await?
                            .context("missing overlay depmap")?;
                        let overlay = self
                            .clone()
                            .spawn_transitive_link(depmap, overlay.dest_subpath, overlay.src_subpath)
                            .await?;
                        overlays.push(overlay);
                    }
                    return Ok(Materialized {
                        direct_path: destination_path,
                        overlays,
                        depmap_hash: depmap.hash().clone(),
                    });
                }
                Err(ref err) if err.kind() == io::ErrorKind::NotFound => {
                    // We need to build
                }
                Err(err) => return Err(err.into()),
            }

            let _build_guard = {
                let span = debug_span!("MaterializeCache::build_lock_acquire");
                let guard = async {
                    match self.building_depmaps.entry(depmap.hash().clone()) {
                        dashmap::mapref::entry::Entry::Occupied(existing) => {
                            // Wait for other task to finish, then retry
                            let lock = existing.get().clone();
                            mem::drop(existing);
                            lock.lock().await;
                            // Retry looking for the stamp file
                            None
                        }
                        dashmap::mapref::entry::Entry::Vacant(entry) => {
                            let lock = Arc::new(futures::lock::Mutex::new(()));
                            let lock_guard = lock.clone().lock_owned().await;
                            // NOTE: we must lock it before putting it into the map
                            entry.insert(lock);
                            Some(BuildingGuard {
                                shared: &self,
                                hash: depmap.hash().clone(),
                                lock_guard,
                            })
                        }
                    }
                }
                .instrument(span)
                .await;
                let Some(guard) = guard else {
                    continue;
                };
                guard
            };

            let build_destination_path = destination_path.with_extension("partial");
            Directory::create_all(&build_destination_path).await?;
            let build_destination = Directory::open(&build_destination_path).await?;

            // FIXME: overlays

            // Use BTree to deduplicate
            let mut entries = BTreeMap::new();
            for entry in depmap.iter() {
                let (k, v) = entry?;
                entries.insert(k, v);
            }
            let entries: Vec<_> = entries.into_iter().collect();
            let mut materialize_futures = FuturesUnordered::new();
            for chunk in entries.chunks(256) {
                let mut build_destination = build_destination.clone()?;
                let this = &*self;
                let materialize_future = async move {
                    for (k, v) in chunk {
                        this.materialize_entry(&mut build_destination, k.as_ref(), v.clone())
                            .await?;
                    }
                    Ok::<(), anyhow::Error>(())
                };
                materialize_futures.push(materialize_future);
            }
            while let Some(()) = materialize_futures.try_next().await? {}

            let overlays: Vec<Overlay> = Vec::new();

            match compio_fs::rename(&build_destination_path, &destination_path).await {
                Ok(()) => {}
                Err(ref err) if err.kind() == io::ErrorKind::DirectoryNotEmpty => {
                    // A previous build might of died after placing the destination path but before writing the stamp
                    // Delete the path and retry
                    compio_fs::remove_dir_all(&destination_path).await?;
                    compio_fs::rename(&build_destination_path, &destination_path).await?;
                }
                Err(err) => return Err(err.into()),
            }

            // Write stamp file
            let mut stamp_file =
                cealn_fs::tempfile(stamp_path.parent().unwrap(), "materialize-stamp-file", false).await?;
            {
                let stamp_file = stamp_file.ensure_open().await?;
                let stamp_bytes = serde_json::to_vec(&Stamp {
                    overlays: overlays
                        .iter()
                        .map(|overlay| StampOverlay {
                            dest_subpath: overlay.dest_subpath.clone(),
                            src_subpath: overlay.src_subpath.clone(),
                            depmap_hash: overlay.materialized.depmap_hash.clone(),
                        })
                        .collect(),
                })?;
                stamp_file.write_all(stamp_bytes).await?;
            }
            cealn_cache::fs::link_into_cache(&mut stamp_file, &stamp_path).await?;

            return Ok(Materialized {
                direct_path: destination_path,
                overlays,
                depmap_hash: depmap.hash().clone(),
            });
        }
    }

    fn spawn_transitive_link(
        self: Arc<Self>,
        transitive_depmap: ConcreteFiletree,
        dest_subpath: String,
        src_subpath: String,
    ) -> RemoteHandle<anyhow::Result<Overlay>> {
        let parent_span = Span::current();
        compio_executor::spawn_handle({
            async move {
                let span = debug_span!(parent: parent_span, "spawn_transitive_link");
                let materialized = self.materialize_internal(transitive_depmap).instrument(span).await?;
                Ok(Overlay {
                    dest_subpath,
                    src_subpath,
                    materialized,
                })
            }
        })
    }

    async fn materialize_entry<'a>(
        &'a self,
        build_destination: &'a mut Directory,
        k: NormalizedDescending<&'a LabelPath>,
        entry: FileEntryRef<'a>,
    ) -> anyhow::Result<()> {
        let mut retry = false;
        loop {
            let result = match entry {
                FileEntryRef::Regular {
                    content_hash,
                    executable,
                } => {
                    let file_guard = self
                        .context
                        .lookup_file(content_hash, executable)
                        .await?
                        .with_context(|| format!("failed to find cache entry for file {:?} -> {:?}", k, entry))?;
                    build_destination.link_at(k, &*file_guard).await
                }
                FileEntryRef::Symlink(target) => build_destination.symlink_at(k, target).await,
                FileEntryRef::Directory => {
                    // FIXME: does this make sense?
                    if k.as_str().is_empty() {
                        return Ok(());
                    }
                    build_destination.create_at_directory(k).await
                }
            };
            match result {
                Ok(()) => return Ok(()),
                Err(ref err) if err.kind() == io::ErrorKind::AlreadyExists => {
                    // This is fine, a partial build may have happened here previously
                    return Ok(());
                }
                Err(ref err) if err.kind() == io::ErrorKind::NotFound && !retry => {
                    // This is most likely caused by a parent directory not existing
                    if let Some(parent) = k.parent() {
                        build_destination.create_at_directory_all(parent).await?;
                        retry = true;
                        continue;
                    } else {
                        bail!("root materialize directory not found for {:?} -> {:?}", k, entry);
                    }
                }
                Err(err) => return Err(anyhow::Error::from(err).context(format!("while building entry at {:?}", k))),
            }
        }
    }
}

struct BuildingGuard<'a> {
    shared: &'a Shared,
    hash: DepmapHash,
    lock_guard: futures::lock::OwnedMutexGuard<()>,
}

impl<'a> Drop for BuildingGuard<'a> {
    fn drop(&mut self) {
        self.shared.building_depmaps.remove(&self.hash);
    }
}
