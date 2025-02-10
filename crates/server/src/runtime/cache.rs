use std::{
    fmt,
    io::{self, SeekFrom},
    sync::Arc,
};

use async_trait::async_trait;
use cealn_cache::hot_disk;
use cealn_runtime::api::{types, Handle, Handle as ApiHandle, HandleRights, Result as WasiResult};
use cealn_runtime_virt::fs::system::RegularFileHandle;

struct CacheFile {
    guard: hot_disk::FileGuard,
    handle: RegularFileHandle,
}

pub async fn open(guard: hot_disk::FileGuard) -> anyhow::Result<Arc<dyn Handle>> {
    let f = CacheFile {
        handle: RegularFileHandle::new(guard.path()).await?,
        guard,
    };
    Ok(Arc::new(f))
}

#[async_trait]
impl Handle for CacheFile {
    fn file_type(&self) -> types::Filetype {
        self.handle.file_type()
    }

    fn rights(&self) -> HandleRights {
        self.handle.rights()
    }

    async fn read(&self, iovs: &mut [io::IoSliceMut]) -> WasiResult<usize> {
        self.handle.read(iovs).await
    }

    async fn write(&self, iovs: &[io::IoSlice]) -> WasiResult<usize> {
        self.handle.write(iovs).await
    }

    async fn tell(&self) -> WasiResult<types::Filesize> {
        self.handle.tell().await
    }

    async fn seek(&self, pos: SeekFrom) -> WasiResult<u64> {
        self.handle.seek(pos).await
    }

    async fn openat_child(
        &self,
        path_segment: &str,
        read: bool,
        write: bool,
        oflags: types::Oflags,
        fd_flags: types::Fdflags,
    ) -> WasiResult<Arc<dyn ApiHandle>> {
        self.handle
            .openat_child(path_segment, read, write, oflags, fd_flags)
            .await
    }

    async fn readdir<'a>(
        &'a self,
        cookie: types::Dircookie,
    ) -> WasiResult<Box<dyn Iterator<Item = WasiResult<(types::Dirent, String)>> + 'a>> {
        self.handle.readdir(cookie).await
    }

    async fn readlinkat_child(&self, path_segment: &str) -> WasiResult<String> {
        self.handle.readlinkat_child(path_segment).await
    }

    async fn filestat(&self) -> WasiResult<types::Filestat> {
        self.handle.filestat().await
    }

    async fn filestat_child(&self, path_segment: &str) -> WasiResult<types::Filestat> {
        self.handle.filestat_child(path_segment).await
    }

    fn fdstat(&self) -> WasiResult<types::Fdflags> {
        self.handle.fdstat()
    }
}

impl fmt::Debug for CacheFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CacheFile").field("path", &self.guard.path()).finish()
    }
}
