cfg_if::cfg_if! {
    if #[cfg(unix)] {
        #[path = "unix.rs"]
        mod imp;
    } else if #[cfg(target_os = "windows")] {
        #[path = "windows.rs"]
        mod imp;
    } else {
        compile_error!("unsupported platform");
    }
}

#[cfg(test)]
mod tests;

use std::{
    fmt, fs,
    io::{self, SeekFrom},
    ops::Deref,
    path::{Path, PathBuf},
    sync::Arc,
};

use async_trait::async_trait;
use thiserror::Error;

use cealn_runtime::api::{
    types::{self, Errno},
    Handle as ApiHandle, HandleRights, Result as WasiResult,
};

/// Implements a read-only WASI filesystem using a underlying host system directory
///
/// This implementation is careful to provide consistent behavior across platforms and to be insensitive to any
/// filesystem differences other than the underlying directory having a different structure or files containing
/// different contents.
///
/// This is only possible in the case where the affected file structure remains unchanged for
/// the lifetime of the file handles pointing to them; if any involved files or directories are changed the behavior
/// is undefined. Cealn handles this by watching the file structure before and after and discarding results if any
/// changes happen while a WASI filesystem is active.
///
/// To facillitate this, the following behaviors are implemented in this layer:
///     * Filename sematics require exact binary matches of filenames, even if the system is case or normalization
///       insensitive. Only valid Unicode (UTF-8 on POSIX, UTF-16 on Windows) filenames are addressable within
///       directories.
///     * Directories are always enumerated in lexical UTF-8 (or equivalently, Unicode code point) order.
///     * Only relative symlinks are allowed; absolute symlinks produce an uncacheable error.
///     * Any errors that are caused by anything other than directory structure and file contents (e.g. permission
///       errors, underlying IO errors) are returned as `Errno::Io` at the WASI layer so caching layers can understand
///       that an uncacheable call has occurred.
pub struct SystemFs {
    root: Arc<DirectoryHandle>,
}

#[derive(Clone)]
pub enum Handle {
    Directory(Arc<DirectoryHandle>),
    RegularFile(Arc<RegularFileHandle>),
}

pub struct DirectoryHandle {
    inner: imp::DirectoryHandle,
}

pub struct RegularFileHandle {
    inner: imp::RegularFileHandle,
}

impl SystemFs {
    pub async fn new(root: PathBuf) -> Result<SystemFs, CreateError> {
        let canonical_root = fs::canonicalize(&root)?;

        let root = DirectoryHandle {
            inner: imp::DirectoryHandle::from_path(&canonical_root).await?,
        };

        Ok(SystemFs { root: Arc::new(root) })
    }

    pub fn root(&self) -> Arc<DirectoryHandle> {
        self.root.clone()
    }
}

impl RegularFileHandle {
    pub async fn new(path: &Path) -> Result<Self, CreateError> {
        let canonical_path = fs::canonicalize(path)?;

        let f = RegularFileHandle {
            inner: imp::RegularFileHandle::from_path(&canonical_path).await?,
        };

        Ok(f)
    }
}

impl Handle {
    pub async fn openat_child(
        &self,
        path_segment: &str,
        read: bool,
        write: bool,
        oflags: types::Oflags,
        fd_flags: types::Fdflags,
    ) -> WasiResult<Handle> {
        match self {
            Handle::Directory(directory) => {
                let handle = directory
                    .inner
                    .openat_child(path_segment, read, write, oflags, fd_flags)
                    .await?;
                Ok(handle)
            }
            Handle::RegularFile(_) => Err(types::Errno::Notdir),
        }
    }
}

#[async_trait]
impl ApiHandle for DirectoryHandle {
    fn file_type(&self) -> types::Filetype {
        types::Filetype::Directory
    }

    fn rights(&self) -> HandleRights {
        HandleRights::new(directory_rights(), inherited_rights())
    }

    async fn read(&self, _iovs: &mut [io::IoSliceMut]) -> WasiResult<usize> {
        return Err(Errno::Isdir);
    }

    async fn write(&self, _iovs: &[io::IoSlice]) -> WasiResult<usize> {
        return Err(Errno::Isdir);
    }

    async fn tell(&self) -> WasiResult<types::Filesize> {
        return Err(Errno::Isdir);
    }

    async fn seek(&self, _pos: SeekFrom) -> WasiResult<u64> {
        return Err(Errno::Isdir);
    }

    async fn openat_child(
        &self,
        path_segment: &str,
        read: bool,
        write: bool,
        oflags: types::Oflags,
        fd_flags: types::Fdflags,
    ) -> WasiResult<Arc<dyn ApiHandle>> {
        let handle = self
            .inner
            .openat_child(path_segment, read, write, oflags, fd_flags)
            .await?;
        Ok(handle.into())
    }

    async fn readdir<'a>(
        &'a self,
        cookie: types::Dircookie,
    ) -> WasiResult<Box<dyn Iterator<Item = WasiResult<(types::Dirent, String)>> + 'a>> {
        self.inner.readdir(cookie).await
    }

    async fn readlinkat_child(&self, path_segment: &str) -> WasiResult<String> {
        self.inner.readlinkat_child(path_segment).await
    }

    async fn filestat(&self) -> WasiResult<types::Filestat> {
        self.inner.filestat().await
    }

    async fn filestat_child(&self, path_segment: &str) -> WasiResult<types::Filestat> {
        self.inner.filestat_child(path_segment).await
    }

    fn fdstat(&self) -> WasiResult<types::Fdflags> {
        Ok(types::Fdflags::empty())
    }
}

#[async_trait]
impl ApiHandle for RegularFileHandle {
    fn file_type(&self) -> types::Filetype {
        types::Filetype::RegularFile
    }

    fn rights(&self) -> HandleRights {
        HandleRights::new(regular_file_rights(), types::Rights::empty())
    }

    async fn read(&self, iovs: &mut [io::IoSliceMut]) -> WasiResult<usize> {
        self.inner.read(iovs).await
    }

    async fn write(&self, _iovs: &[io::IoSlice]) -> WasiResult<usize> {
        return Err(Errno::Notcapable);
    }

    async fn tell(&self) -> WasiResult<types::Filesize> {
        self.inner.tell().await
    }

    async fn seek(&self, pos: SeekFrom) -> WasiResult<u64> {
        self.inner.seek(pos).await
    }

    async fn openat_child(
        &self,
        _path_segment: &str,
        _read: bool,
        _write: bool,
        _oflags: types::Oflags,
        _fd_flags: types::Fdflags,
    ) -> WasiResult<Arc<dyn ApiHandle>> {
        return Err(Errno::Notdir);
    }

    async fn readdir<'a>(
        &'a self,
        _cookie: types::Dircookie,
    ) -> WasiResult<Box<dyn Iterator<Item = WasiResult<(types::Dirent, String)>> + 'a>> {
        return Err(Errno::Notdir);
    }

    async fn readlinkat_child(&self, _path_segment: &str) -> WasiResult<String> {
        return Err(Errno::Notdir);
    }

    async fn filestat(&self) -> WasiResult<types::Filestat> {
        self.inner.filestat().await
    }

    async fn filestat_child(&self, _path_segment: &str) -> WasiResult<types::Filestat> {
        return Err(Errno::Notdir);
    }

    fn fdstat(&self) -> WasiResult<types::Fdflags> {
        Ok(types::Fdflags::empty())
    }
}

fn regular_file_rights() -> types::Rights {
    return types::Rights::FD_READ | types::Rights::FD_SEEK | types::Rights::FD_TELL | types::Rights::FD_FILESTAT_GET;
}
fn directory_rights() -> types::Rights {
    return types::Rights::PATH_OPEN
        | types::Rights::FD_READDIR
        | types::Rights::PATH_FILESTAT_GET
        | types::Rights::FD_FILESTAT_GET;
}

fn inherited_rights() -> types::Rights {
    return regular_file_rights() | directory_rights();
}

impl From<Arc<DirectoryHandle>> for Handle {
    fn from(x: Arc<DirectoryHandle>) -> Self {
        Handle::Directory(x)
    }
}

impl From<Arc<RegularFileHandle>> for Handle {
    fn from(x: Arc<RegularFileHandle>) -> Self {
        Handle::RegularFile(x)
    }
}

impl Into<Arc<dyn ApiHandle>> for Handle {
    fn into(self) -> Arc<dyn ApiHandle> {
        match self {
            Handle::Directory(dir) => dir,
            Handle::RegularFile(f) => f,
        }
    }
}

impl Deref for Handle {
    type Target = dyn ApiHandle;

    #[inline]
    fn deref(&self) -> &Self::Target {
        match self {
            Handle::Directory(handle) => &**handle,
            Handle::RegularFile(handle) => &**handle,
        }
    }
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Handle::Directory(handle) => fmt::Debug::fmt(&handle.inner, f),
            Handle::RegularFile(handle) => fmt::Debug::fmt(&handle.inner, f),
        }
    }
}

impl fmt::Debug for DirectoryHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

impl fmt::Debug for RegularFileHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

#[derive(Error, Debug)]
pub enum CreateError {
    #[error("the provided root path must be an absolute path")]
    RelativeRootPath,
    #[error("IO error encountered when initializing system filesystem: {0}")]
    Io(#[from] io::Error),
}
