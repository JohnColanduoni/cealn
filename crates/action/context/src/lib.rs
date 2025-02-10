use std::{ops::Deref, path::Path};

use async_trait::async_trait;
use cealn_fs_materialize::MaterializeCache;
use futures::{future::BoxFuture, prelude::*};

use tempfile::TempDir;

use cealn_data::{
    depmap::{ConcreteDepmapReference, DepmapHash},
    file_entry::{FileEntry, FileHash, FileHashRef},
    label::{LabelPathBuf, NormalizedDescending},
    LabelBuf,
};
use cealn_depset::{depmap::DepMap, ConcreteFiletree, LabelFiletree};
use cealn_event::EventContext;
use cealn_fs::Cachefile;

pub use reqwest;

#[async_trait]
pub trait Context: Send + Sync + Clone + 'static {
    type CacheFileGuard: Deref<Target = Path> + Send + Sync;
    type ProcessTicket: Send;

    fn events(&self) -> &EventContext;

    fn http_client(&self) -> &reqwest::Client;

    fn spawn_immediate<F>(&self, f: F) -> future::RemoteHandle<<F as Future>::Output>
    where
        F: Future + Send + 'static,
        <F as Future>::Output: Send;

    /// Creates an unnamed temporary file
    ///
    /// This file will be eligible to be linked into the hot cache.
    async fn tempfile(&self, description: &str, executable: bool) -> anyhow::Result<Cachefile>;

    async fn tempdir(&self, description: &str) -> anyhow::Result<TempDir>;

    fn tempdir_root(&self) -> &Path;

    /// Moves a temporary file into the cache
    async fn move_to_cache(&self, file: Cachefile) -> anyhow::Result<(FileHash, bool)>;

    async fn move_to_cache_named(&self, path: &Path) -> anyhow::Result<(FileHash, bool)>;

    fn primary_cache_dir(&self) -> &Path;

    /// Moves a temporary file into the cache
    ///
    /// This version is most useful when the action has an oppurtunity to efficiently hash the file during its
    /// creation, so reading the file again can be avoided.
    async fn move_to_cache_prehashed(
        &self,
        file: Cachefile,
        digest: FileHashRef<'_>,
        executable: bool,
    ) -> anyhow::Result<()>;

    /// Adds a [`DepMap`] that maps file paths to [`FileEntry`]s
    async fn register_concrete_filetree_depmap(&self, depmap: ConcreteFiletree) -> anyhow::Result<DepmapHash>;

    /// Adds a [`DepMap`] that maps file paths to [`LabelBuf`]s
    fn register_label_filetree_depmap(&self, depmap: LabelFiletree) -> anyhow::Result<DepmapHash>;

    async fn lookup_concrete_depmap(&self, hash: &ConcreteDepmapReference) -> anyhow::Result<ConcreteDepmapResolution>;

    async fn lookup_concrete_depmap_force_directory(
        &self,
        hash: &ConcreteDepmapReference,
    ) -> anyhow::Result<ConcreteFiletree>;

    async fn open_cache_file(
        &self,
        content_hash: FileHashRef<'_>,
        executable: bool,
    ) -> anyhow::Result<Self::CacheFileGuard>;

    async fn open_depmap_file(&self, reference: &ConcreteDepmapReference) -> anyhow::Result<Self::CacheFileGuard>;

    fn materialize_cache(&self) -> &MaterializeCache;

    fn acquire_process_ticket<'a>(&'a self) -> BoxFuture<'a, anyhow::Result<Self::ProcessTicket>>;
}

#[derive(Clone)]
pub enum ConcreteDepmapResolution {
    Depmap(ConcreteFiletree),
    Subpath(ConcreteFiletree, NormalizedDescending<LabelPathBuf>),
}
