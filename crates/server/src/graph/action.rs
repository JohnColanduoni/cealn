use std::{fs::File, path::Path, sync::Arc};

use anyhow::{anyhow, bail, Context as _};
use async_trait::async_trait;
use futures::prelude::*;
use tempfile::TempDir;

use cealn_action_context::{reqwest, ConcreteDepmapResolution};
use cealn_cache::hot_disk;
use cealn_data::{
    depmap::{ConcreteDepmapReference, DepmapHash},
    file_entry::{FileEntry, FileEntryRef, FileHash, FileHashRef},
    LabelBuf,
};
use cealn_depset::{ConcreteFiletree, DepMap, LabelFiletree};
use cealn_event::EventContext;
use cealn_fs::Cachefile;
use cealn_fs_materialize::MaterializeCache;

use crate::{executor::ProcessTicket, graph::graph::_Graph};

pub(crate) struct Context {
    graph: Arc<_Graph>,
    events: EventContext,
}

impl Context {
    pub(crate) fn new(graph: Arc<_Graph>, events: EventContext) -> Self {
        Context { graph, events }
    }
}

#[async_trait]
impl cealn_action::Context for Context {
    type CacheFileGuard = hot_disk::FileGuard;
    type ProcessTicket = ProcessTicket;

    #[inline]
    fn events(&self) -> &EventContext {
        &self.events
    }

    #[inline]
    fn http_client(&self) -> &reqwest::Client {
        &self.graph.http_client
    }

    fn spawn_immediate<F>(&self, f: F) -> futures::future::RemoteHandle<<F as futures::Future>::Output>
    where
        F: futures::Future + Send + 'static,
        <F as futures::Future>::Output: Send,
    {
        self.graph.executor.spawn_immediate(f)
    }

    async fn tempfile(&self, description: &str, executable: bool) -> anyhow::Result<Cachefile> {
        cealn_fs::tempfile(&self.graph.temporary_directory, description, executable).await
    }

    async fn tempdir(&self, description: &str) -> anyhow::Result<TempDir> {
        let tempdir = tempfile::Builder::new()
            .prefix(description)
            .tempdir_in(&self.graph.temporary_directory)?;
        Ok(tempdir)
    }

    fn tempdir_root(&self) -> &Path {
        &self.graph.temporary_directory
    }

    async fn move_to_cache(&self, file: Cachefile) -> anyhow::Result<(FileHash, bool)> {
        self.graph.cache_subsystem.primary_cache.move_to_cache(file).await
    }

    async fn move_to_cache_prehashed(
        &self,
        file: Cachefile,
        digest: FileHashRef<'_>,
        executable: bool,
    ) -> anyhow::Result<()> {
        self.graph
            .cache_subsystem
            .primary_cache
            .move_to_cache_prehashed(file, digest, executable)
            .await
    }

    async fn move_to_cache_named(&self, path: &Path) -> anyhow::Result<(FileHash, bool)> {
        self.graph
            .cache_subsystem
            .primary_cache
            .move_to_cache_named(path, true)
            .await
    }

    fn primary_cache_dir(&self) -> &Path {
        self.graph.cache_subsystem.primary_cache.root()
    }

    async fn register_concrete_filetree_depmap(&self, depmap: ConcreteFiletree) -> anyhow::Result<DepmapHash> {
        self.graph.register_filetree(depmap).await
    }

    async fn lookup_concrete_depmap(
        &self,
        reference: &ConcreteDepmapReference,
    ) -> anyhow::Result<ConcreteDepmapResolution> {
        let depmap = self
            .graph
            .lookup_filetree_cache(&reference.hash)
            .await?
            .ok_or_else(|| anyhow!("missing concrete depmap"))?;
        match &reference.subpath {
            Some(subpath) => Ok(ConcreteDepmapResolution::Subpath(depmap, subpath.to_owned())),
            None => Ok(ConcreteDepmapResolution::Depmap(depmap)),
        }
    }

    async fn lookup_concrete_depmap_force_directory(
        &self,
        reference: &ConcreteDepmapReference,
    ) -> anyhow::Result<ConcreteFiletree> {
        let depmap = self
            .graph
            .lookup_filetree_cache(&reference.hash)
            .await?
            .ok_or_else(|| anyhow!("missing concrete depmap"))?;
        match &reference.subpath {
            Some(subpath) => {
                let mut new_depmap = DepMap::builder();
                // FIXME: cache?
                for entry in depmap.iter() {
                    let (k, v) = entry?;
                    let Some(new_path) = k.strip_prefix(subpath) else {
                        continue
                    };
                    new_depmap.insert(new_path, v);
                }
                Ok(new_depmap.build())
            }
            None => Ok(depmap),
        }
    }

    fn register_label_filetree_depmap(&self, depmap: LabelFiletree) -> anyhow::Result<DepmapHash> {
        Ok(self
            .graph
            .cache_subsystem
            .depset_registry
            .register_label_filetree(depmap))
    }

    async fn open_cache_file(
        &self,
        content_hash: FileHashRef<'_>,
        executable: bool,
    ) -> anyhow::Result<hot_disk::FileGuard> {
        self.graph
            .open_cache_file(content_hash, executable)
            .await?
            .context("missing content hash in cache")
    }

    async fn open_depmap_file(&self, reference: &ConcreteDepmapReference) -> anyhow::Result<hot_disk::FileGuard> {
        let depmap = self
            .graph
            .lookup_filetree_cache(&reference.hash)
            .await?
            .ok_or_else(|| anyhow!("requested unresolved concrete depmap"))?;
        let Some(subpath) = &reference.subpath else {
            bail!("expected single file input");
        };
        match depmap.get(subpath.as_ref())? {
            Some(FileEntryRef::Regular {
                content_hash,
                executable,
            }) => self.open_cache_file(content_hash, executable).await,
            None => bail!("missing file {}", subpath),
            _ => todo!(),
        }
    }

    fn materialize_cache(&self) -> &MaterializeCache {
        &self.graph.materialize_cache
    }

    fn acquire_process_ticket<'a>(&'a self) -> futures::future::BoxFuture<'a, anyhow::Result<Self::ProcessTicket>> {
        self.graph.executor.acquire_process_ticket().boxed()
    }
}

impl Clone for Context {
    fn clone(&self) -> Self {
        Context {
            graph: self.graph.clone(),
            events: self.events.fork(),
        }
    }
}
