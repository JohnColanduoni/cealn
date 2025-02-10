use std::{
    cell::UnsafeCell,
    ffi::CString,
    fs::File as StdFile,
    io::Result,
    mem::{self, ManuallyDrop},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::prelude::OsStrExt,
    },
    path::{Path, PathBuf},
    pin::Pin,
    ptr,
    sync::Arc,
};

use compio_core::{
    buffer::{self, RawInputBuffer, RawOutputBuffer},
    io_uring::{CompletionWakerStorage, CurrentEventQueueSubmitterSource},
};
use io_uring::opcode;
use pin_project::pin_project;

use crate::{directory::DirectoryOpenOptions, OpenOptions, platform::statx};

pub(super) struct File {
    data: ManuallyDrop<Pin<Box<FileOpData>>>,
}

#[pin_project]
struct FileOpData {
    #[pin]
    callback: CompletionWakerStorage,
    statx: UnsafeCell<statx>,
}

impl Drop for File {
    fn drop(&mut self) {
        unsafe {
            // Delay release of the allocation holding the callback if there is a pending operation
            let callback_ptr = self.data.as_mut().project().callback.get_unchecked_mut() as *mut _;
            let allocation = ManuallyDrop::take(&mut self.data);
            CompletionWakerStorage::ensure_cleanup(callback_ptr, move || mem::drop(allocation));
        }
    }
}

unsafe impl Send for File {}
unsafe impl Sync for File {}

pub(super) struct Directory {
    data: ManuallyDrop<Pin<Box<DirectoryOpData>>>,
}

#[pin_project]
struct DirectoryOpData {
    #[pin]
    callback: CompletionWakerStorage,
    path_buf: UnsafeCell<Vec<u8>>,
    path_buf2: UnsafeCell<Vec<u8>>,
}

impl Drop for Directory {
    fn drop(&mut self) {
        unsafe {
            // Delay release of the allocation holding the callback if there is a pending operation
            let callback_ptr = self.data.as_mut().project().callback.get_unchecked_mut() as *mut _;
            let allocation = ManuallyDrop::take(&mut self.data);
            CompletionWakerStorage::ensure_cleanup(callback_ptr, move || mem::drop(allocation));
        }
    }
}

unsafe impl Send for Directory {}
unsafe impl Sync for Directory {}

struct FreestandingOperation {
    // This must be kept alive as long as there is a pending operation, so we prevent it from being automatically dropped
    data: ManuallyDrop<Pin<Box<FreestandingOperationData>>>,
}

impl Drop for FreestandingOperation {
    fn drop(&mut self) {
        unsafe {
            // Delay release of the allocation holding the callback if there is a pending operation
            let callback_ptr = self.data.as_mut().project().callback.get_unchecked_mut() as *mut _;
            let allocation = ManuallyDrop::take(&mut self.data);
            CompletionWakerStorage::ensure_cleanup(callback_ptr, move || mem::drop(allocation));
        }
    }
}

#[pin_project]
struct FreestandingOperationData {
    #[pin]
    callback: CompletionWakerStorage,
    path_buf: UnsafeCell<Vec<u8>>,
    path_buf2: UnsafeCell<Vec<u8>>,
    statx: UnsafeCell<statx>,
}

impl File {
    pub(super) fn new() -> Self {
        unsafe {
            File {
                data: ManuallyDrop::new(Box::pin(FileOpData {
                    callback: CompletionWakerStorage::new(),
                    statx: UnsafeCell::new(mem::zeroed()),
                })),
            }
        }
    }

    pub(super) async fn open_with_options(options: &OpenOptions, path: &Path) -> Result<super::File> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let mut openat = FreestandingOperation::new();
            let data = openat.data.as_mut().project();
            let callback = data.callback;

            let fd = callback
                .submit(&mut submitter, {
                    move || {
                        let path_storage = &mut *data.path_buf.get();
                        path_storage.clear();
                        path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(path.as_os_str().as_bytes());
                        path_storage.push(b'\0');
                        opcode::OpenAt::new(io_uring::types::Fd(libc::AT_FDCWD), path_storage.as_ptr() as _)
                            .flags(options.to_flags())
                            .mode(options.get_mode())
                            .build()
                    }
                })
                .await?;

            Ok(super::File {
                fd: StdFile::from_raw_fd(fd),
                imp: super::FileImp::IoUring(File::new()),
            })
        }
    }

    pub async fn read<'a>(&mut self, fd: &'a StdFile, buffer: impl RawInputBuffer + 'a) -> Result<usize> {
        unsafe {
            // FIXME: allow pinned vectorized buffers here without copies
            let mut buffer = buffer::ensure_pinned_input(buffer);
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            let fd = fd.as_raw_fd();

            // FIXME: eventually free pinned buffer on cancel
            match callback
                .submit(&mut submitter, {
                    let buffer = &mut buffer;
                    move || {
                        opcode::Read::new(
                            io_uring::types::Fd(fd),
                            buffer.as_mut_ptr() as *mut u8,
                            buffer.len() as u32,
                        )
                        .offset(u64::MAX)
                        .build()
                    }
                })
                .await
            {
                Ok(bytes_read) => {
                    buffer.finalize(bytes_read as usize);
                    Ok(bytes_read as usize)
                }
                Err(err) => {
                    buffer.release();
                    Err(err)
                }
            }
        }
    }

    pub async fn write<'a>(&mut self, fd: &'a StdFile, buffer: impl RawOutputBuffer + 'a) -> Result<usize> {
        unsafe {
            // FIXME: allow pinned vectorized buffers here without copies
            let mut buffer = buffer::ensure_pinned_output(buffer);
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            let fd = fd.as_raw_fd();

            // FIXME: eventually free pinned buffer on cancel
            match callback
                .submit(&mut submitter, {
                    let buffer = &mut buffer;
                    move || {
                        opcode::Write::new(
                            io_uring::types::Fd(fd),
                            buffer.as_ptr() as *const u8,
                            buffer.len() as u32,
                        )
                        .offset(u64::MAX)
                        .build()
                    }
                })
                .await
            {
                Ok(bytes_written) => {
                    buffer.release();
                    Ok(bytes_written as usize)
                }
                Err(err) => {
                    buffer.release();
                    Err(err)
                }
            }
        }
    }

    pub async fn symlink_metadata<'a>(&mut self, fd: &'a StdFile) -> Result<super::Metadata> {
        unsafe {
            // FIXME: allow pinned vectorized buffers here without copies
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            let fd = fd.as_raw_fd();

            match callback
                .submit(&mut submitter, {
                    move || {
                        let statx = &mut *data.statx.get();
                        opcode::Statx::new(
                            io_uring::types::Fd(fd),
                            EMPTY_FILENAME.as_ptr() as _,
                            statx as *mut statx as *mut ::io_uring::types::statx,
                        )
                        .flags(libc::AT_EMPTY_PATH | libc::AT_SYMLINK_NOFOLLOW)
                        .build()
                    }
                })
                .await
            {
                Ok(_) => Ok(super::Metadata {
                    stat: *self.data.as_mut().project().statx.get(),
                }),
                Err(err) => Err(err),
            }
        }
    }
}

static EMPTY_FILENAME: &'static [u8] = b"\0";

impl Directory {
    pub(super) fn clone(&self) -> Self {
        Self::new()
    }

    pub(super) fn new() -> Self {
        Directory {
            data: ManuallyDrop::new(Box::pin(DirectoryOpData {
                callback: CompletionWakerStorage::new(),
                path_buf: Default::default(),
                path_buf2: Default::default(),
            })),
        }
    }

    pub(super) async fn open(path: &Path) -> Result<super::Directory> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let mut openat = FreestandingOperation::new();
            let data = openat.data.as_mut().project();
            let callback = data.callback;

            let fd = callback
                .submit(&mut submitter, {
                    move || {
                        let path_storage = &mut *data.path_buf.get();
                        path_storage.clear();
                        path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(path.as_os_str().as_bytes());
                        path_storage.push(b'\0');
                        opcode::OpenAt::new(io_uring::types::Fd(libc::AT_FDCWD), path_storage.as_ptr() as _)
                            .flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_RDONLY)
                            .build()
                    }
                })
                .await?;

            Ok(super::Directory {
                fd: Arc::new(StdFile::from_raw_fd(fd)),
                imp: super::DirectoryImp::IoUring(Self::new()),
            })
        }
    }

    pub(super) async fn create(path: &Path) -> Result<()> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let mut mkdirat = FreestandingOperation::new();
            let data = mkdirat.data.as_mut().project();
            let callback = data.callback;

            callback
                .submit(&mut submitter, {
                    move || {
                        let path_storage = &mut *data.path_buf.get();
                        path_storage.clear();
                        path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(path.as_os_str().as_bytes());
                        path_storage.push(b'\0');
                        opcode::MkDirAt::new(io_uring::types::Fd(libc::AT_FDCWD), path_storage.as_ptr() as _)
                            .mode(0o777)
                            .build()
                    }
                })
                .await?;

            Ok(())
        }
    }

    pub(super) async fn open_at_file_with_options(
        &mut self,
        fd: &StdFile,
        options: &OpenOptions,
        path: &Path,
    ) -> Result<super::File> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            // Don't dereference path buffer until we know we have unique access to it; an ongoing operation may be in
            // progress until the prepare callback of `submit` below.
            let path_storage = data.path_buf;
            let fd = fd.as_raw_fd();

            let fd = callback
                .submit(&mut submitter, {
                    move || {
                        // We now know we have exclusive access to the path storage
                        let path_storage = &mut *path_storage.get();
                        path_storage.clear();
                        path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(path.as_os_str().as_bytes());
                        path_storage.push(b'\0');

                        opcode::OpenAt::new(io_uring::types::Fd(fd), path_storage.as_ptr() as _)
                            .flags(options.to_flags())
                            .mode(options.get_mode())
                            .build()
                    }
                })
                .await?;

            Ok(super::File {
                fd: StdFile::from_raw_fd(fd),
                imp: super::FileImp::IoUring(File {
                    data: ManuallyDrop::new(Box::pin(FileOpData {
                        callback: CompletionWakerStorage::new(),
                        statx: UnsafeCell::new(mem::zeroed()),
                    })),
                }),
            })
        }
    }

    pub(super) async fn open_at_directory(&mut self, fd: &StdFile, path: &Path) -> Result<super::Directory> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            // Don't dereference path buffer until we know we have unique access to it; an ongoing operation may be in
            // progress until the prepare callback of `submit` below.
            let fd = fd.as_raw_fd();

            let fd = callback
                .submit(&mut submitter, {
                    move || {
                        // We now know we have exclusive access to the path storage
                        let path_storage = &mut *data.path_buf.get();
                        path_storage.clear();
                        path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(path.as_os_str().as_bytes());
                        path_storage.push(b'\0');

                        opcode::OpenAt::new(io_uring::types::Fd(fd), path_storage.as_ptr() as _)
                            .flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_RDONLY)
                            .build()
                    }
                })
                .await?;

            Ok(super::Directory {
                fd: Arc::new(StdFile::from_raw_fd(fd)),
                imp: super::DirectoryImp::IoUring(Self::new()),
            })
        }
    }

    pub(super) async fn create_at_directory(&mut self, fd: &StdFile, path: &Path) -> Result<()> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            // Don't dereference path buffer until we know we have unique access to it; an ongoing operation may be in
            // progress until the prepare callback of `submit` below.
            let fd = fd.as_raw_fd();

            callback
                .submit(&mut submitter, {
                    move || {
                        // We now know we have exclusive access to the path storage
                        let path_storage = &mut *data.path_buf.get();
                        path_storage.clear();
                        path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(path.as_os_str().as_bytes());
                        path_storage.push(b'\0');

                        opcode::MkDirAt::new(io_uring::types::Fd(fd), path_storage.as_ptr() as _)
                            .mode(0o777)
                            .build()
                    }
                })
                .await?;

            Ok(())
        }
    }

    pub(super) async fn link_at(&mut self, fd: &StdFile, link_path: &Path, target_path: &Path) -> Result<()> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            // Don't dereference path buffer until we know we have unique access to it; an ongoing operation may be in
            // progress until the prepare callback of `submit` below.
            let fd = fd.as_raw_fd();

            callback
                .submit(&mut submitter, {
                    move || {
                        // We now know we have exclusive access to the path storage
                        let path_storage = &mut *data.path_buf.get();
                        path_storage.clear();
                        path_storage.reserve(link_path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(link_path.as_os_str().as_bytes());
                        path_storage.push(b'\0');
                        let path_storage2 = &mut *data.path_buf2.get();
                        path_storage2.clear();
                        path_storage2.reserve(target_path.as_os_str().as_bytes().len() + 1);
                        path_storage2.extend_from_slice(target_path.as_os_str().as_bytes());
                        path_storage2.push(b'\0');

                        opcode::LinkAt::new(
                            io_uring::types::Fd(libc::AT_FDCWD),
                            path_storage2.as_ptr() as _,
                            io_uring::types::Fd(fd),
                            path_storage.as_ptr() as _,
                        )
                        .build()
                    }
                })
                .await?;

            Ok(())
        }
    }

    pub(super) async fn symlink_at(&mut self, fd: &StdFile, link_path: &Path, target_path: &Path) -> Result<()> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            // Don't dereference path buffer until we know we have unique access to it; an ongoing operation may be in
            // progress until the prepare callback of `submit` below.
            let fd = fd.as_raw_fd();

            callback
                .submit(&mut submitter, {
                    move || {
                        // We now know we have exclusive access to the path storage
                        let path_storage = &mut *data.path_buf.get();
                        path_storage.clear();
                        path_storage.reserve(link_path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(link_path.as_os_str().as_bytes());
                        path_storage.push(b'\0');
                        let path_storage2 = &mut *data.path_buf2.get();
                        path_storage2.clear();
                        path_storage2.reserve(target_path.as_os_str().as_bytes().len() + 1);
                        path_storage2.extend_from_slice(target_path.as_os_str().as_bytes());
                        path_storage2.push(b'\0');

                        opcode::SymlinkAt::new(
                            io_uring::types::Fd(fd),
                            path_storage2.as_ptr() as _,
                            path_storage.as_ptr() as _,
                        )
                        .build()
                    }
                })
                .await?;

            Ok(())
        }
    }

    pub(super) async fn unlink_at(&mut self, fd: &StdFile, path: &Path) -> Result<()> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            // Don't dereference path buffer until we know we have unique access to it; an ongoing operation may be in
            // progress until the prepare callback of `submit` below.
            let fd = fd.as_raw_fd();

            callback
                .submit(&mut submitter, {
                    move || {
                        // We now know we have exclusive access to the path storage
                        let path_storage = &mut *data.path_buf.get();
                        path_storage.clear();
                        path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(path.as_os_str().as_bytes());
                        path_storage.push(b'\0');

                        opcode::UnlinkAt::new(io_uring::types::Fd(fd), path_storage.as_ptr() as _).build()
                    }
                })
                .await?;

            Ok(())
        }
    }

    pub(super) async fn remove_dir_at(&mut self, fd: &StdFile, path: &Path) -> Result<()> {
        unsafe {
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            // Don't dereference path buffer until we know we have unique access to it; an ongoing operation may be in
            // progress until the prepare callback of `submit` below.
            let fd = fd.as_raw_fd();

            callback
                .submit(&mut submitter, {
                    move || {
                        // We now know we have exclusive access to the path storage
                        let path_storage = &mut *data.path_buf.get();
                        path_storage.clear();
                        path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                        path_storage.extend_from_slice(path.as_os_str().as_bytes());
                        path_storage.push(b'\0');

                        opcode::UnlinkAt::new(io_uring::types::Fd(fd), path_storage.as_ptr() as _)
                            .flags(libc::AT_REMOVEDIR)
                            .build()
                    }
                })
                .await?;

            Ok(())
        }
    }
}

pub(super) async fn remove_file(path: &Path) -> Result<()> {
    unsafe {
        let mut submitter = CurrentEventQueueSubmitterSource;
        let mut renameat = FreestandingOperation::new();
        let data = renameat.data.as_mut().project();
        let callback = data.callback;

        callback
            .submit(&mut submitter, {
                move || {
                    // We now know we have exclusive access to the path storage
                    let path_storage = &mut *data.path_buf.get();
                    path_storage.clear();
                    path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                    path_storage.extend_from_slice(path.as_os_str().as_bytes());
                    path_storage.push(b'\0');

                    opcode::UnlinkAt::new(io_uring::types::Fd(libc::AT_FDCWD), path_storage.as_ptr() as _).build()
                }
            })
            .await?;

        Ok(())
    }
}

pub(super) async fn rename(src: &Path, dest: &Path) -> Result<()> {
    unsafe {
        let mut submitter = CurrentEventQueueSubmitterSource;
        let mut renameat = FreestandingOperation::new();
        let data = renameat.data.as_mut().project();
        let callback = data.callback;

        callback
            .submit(&mut submitter, {
                move || {
                    // We now know we have exclusive access to the path storage
                    let path_storage = &mut *data.path_buf.get();
                    path_storage.clear();
                    path_storage.reserve(src.as_os_str().as_bytes().len() + 1);
                    path_storage.extend_from_slice(src.as_os_str().as_bytes());
                    path_storage.push(b'\0');
                    let path_storage2 = &mut *data.path_buf2.get();
                    path_storage2.clear();
                    path_storage2.reserve(dest.as_os_str().as_bytes().len() + 1);
                    path_storage2.extend_from_slice(dest.as_os_str().as_bytes());
                    path_storage2.push(b'\0');

                    opcode::RenameAt::new(
                        io_uring::types::Fd(libc::AT_FDCWD),
                        path_storage.as_ptr() as _,
                        io_uring::types::Fd(libc::AT_FDCWD),
                        path_storage2.as_ptr() as _,
                    )
                    .build()
                }
            })
            .await?;

        Ok(())
    }
}

pub(super) async fn symlink_metadata(path: &Path) -> Result<super::Metadata> {
    unsafe {
        let mut submitter = CurrentEventQueueSubmitterSource;
        let mut statx = FreestandingOperation::new();
        let data = statx.data.as_mut().project();
        let callback = data.callback;

        match callback
            .submit(&mut submitter, {
                move || {
                    // We now know we have exclusive access to the path storage
                    let path_storage = &mut *data.path_buf.get();
                    path_storage.clear();
                    path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                    path_storage.extend_from_slice(path.as_os_str().as_bytes());
                    path_storage.push(b'\0');
                    let statx = &mut *data.statx.get();
                    opcode::Statx::new(
                        io_uring::types::Fd(libc::AT_FDCWD),
                        path_storage.as_ptr() as _,
                        statx as *mut statx as *mut ::io_uring::types::statx,
                    )
                    .flags(libc::AT_SYMLINK_NOFOLLOW)
                    .build()
                }
            })
            .await
        {
            Ok(_) => Ok(super::Metadata {
                stat: *statx.data.as_mut().project().statx.get(),
            }),
            Err(err) => Err(err),
        }
    }
}

pub(super) async fn remove_dir(path: &Path) -> Result<()> {
    unsafe {
        let mut submitter = CurrentEventQueueSubmitterSource;
        let mut unlinkat = FreestandingOperation::new();
        let data = unlinkat.data.as_mut().project();
        let callback = data.callback;

        callback
            .submit(&mut submitter, {
                move || {
                    // We now know we have exclusive access to the path storage
                    let path_storage = &mut *data.path_buf.get();
                    path_storage.clear();
                    path_storage.reserve(path.as_os_str().as_bytes().len() + 1);
                    path_storage.extend_from_slice(path.as_os_str().as_bytes());
                    path_storage.push(b'\0');
                    opcode::UnlinkAt::new(io_uring::types::Fd(libc::AT_FDCWD), path_storage.as_ptr() as _)
                        .flags(libc::AT_REMOVEDIR)
                        .build()
                }
            })
            .await?;
        Ok(())
    }
}

impl FreestandingOperation {
    fn new() -> FreestandingOperation {
        unsafe {
            FreestandingOperation {
                data: ManuallyDrop::new(Box::pin(FreestandingOperationData {
                    callback: CompletionWakerStorage::new(),
                    path_buf: Default::default(),
                    path_buf2: Default::default(),
                    statx: mem::zeroed(),
                })),
            }
        }
    }
}
