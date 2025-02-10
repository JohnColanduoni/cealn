#![feature(io_error_more)]

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        #[path = "platform/linux.rs"]
        mod platform;
    } else if #[cfg(target_os = "macos")] {
        #[path = "platform/macos.rs"]
        mod platform;
    } else {
        compile_error!("unsupported platform");
    }
}

pub use crate::platform::{materialize_for_output, MaterializeCache, Materialized};

use async_trait::async_trait;
use cealn_cache::hot_disk::FileGuard;
use cealn_data::{
    depmap::DepmapHash,
    file_entry::{FileEntry, FileHashRef},
};
use cealn_depset::{ConcreteFiletree, DepMap};

#[async_trait]
pub trait MaterializeContext: Send + Sync {
    async fn lookup_file<'a>(&'a self, digest: FileHashRef<'a>, executable: bool) -> anyhow::Result<Option<FileGuard>>;
    async fn lookup_filetree_cache<'a>(&'a self, hash: &'a DepmapHash) -> anyhow::Result<Option<ConcreteFiletree>>;
}
