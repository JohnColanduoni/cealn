use std::{
    convert::TryFrom,
    fmt,
    io::{self, SeekFrom},
    os::unix::{io::RawFd, prelude::*},
    path::Path,
    sync::Arc,
};

use compio_core::os::unix::UnpinnedIoVecMut;
use compio_fs::{Directory, File, Metadata, OpenOptions};
use futures::{lock::Mutex as AsyncMutex, prelude::*};
use tracing::error;

#[cfg(target_os = "linux")]
use compio_fs::os::linux::OpenOptionsExt;
#[cfg(target_os = "macos")]
use compio_fs::os::macos::OpenOptionsExt;

use cealn_runtime::api::{types, Result};

use super::CreateError;

pub struct DirectoryHandle {
    dir: AsyncMutex<Directory>,
}

pub struct RegularFileHandle {
    file: AsyncMutex<File>,
}

impl DirectoryHandle {
    pub(super) async fn from_path(path: &Path) -> std::result::Result<Self, CreateError> {
        let dir = Directory::open(path).await?;

        Ok(DirectoryHandle {
            dir: AsyncMutex::new(dir),
        })
    }
}

impl RegularFileHandle {
    pub(super) async fn from_path(path: &Path) -> std::result::Result<Self, CreateError> {
        let file = OpenOptions::new().read(true).nofollow(true).open(path).await?;

        Ok(RegularFileHandle {
            file: AsyncMutex::new(file),
        })
    }
}

impl DirectoryHandle {
    pub(super) async fn openat_child(
        &self,
        path_segment: &str,
        _read: bool,
        write: bool,
        oflags: types::Oflags,
        fd_flags: types::Fdflags,
    ) -> Result<super::Handle> {
        if write
            || oflags.contains(types::Oflags::CREAT)
            || oflags.contains(types::Oflags::EXCL)
            || oflags.contains(types::Oflags::TRUNC)
            || fd_flags.contains(types::Fdflags::APPEND)
            || fd_flags.contains(types::Fdflags::DSYNC)
            || fd_flags.contains(types::Fdflags::RSYNC)
            || fd_flags.contains(types::Fdflags::SYNC)
        {
            // NOTE: wasi libc seems to sometimes open with nonblock here which doesn't make much sense, but doesn't
            // hurt anything I guess since it's not a socket.
            error!(?oflags, ?fd_flags, "attempted to open file with mutating oflag");
            return Err(types::Errno::Notcapable);
        }

        if path_segment.chars().any(|x| x == '/' || x == '\\' || x == '\0') {
            return Err(types::Errno::Ilseq);
        }

        if oflags.contains(types::Oflags::DIRECTORY) {
            let mut dir = self.dir.lock().await;
            // FIXME: make sure this handles symlinks correctly (i.e. it doesn't follow them)
            let subdir = match dir.open_at_directory(path_segment).await {
                Ok(dir) => dir,
                Err(ref err) if err.raw_os_error() == Some(libc::ENOENT) => return Err(types::Errno::Noent),
                Err(ref err) if err.raw_os_error() == Some(libc::ENOTDIR) => return Err(types::Errno::Notdir),
                Err(_error) => {
                    return Err(types::Errno::Io);
                }
            };
            Ok(super::Handle::Directory(Arc::new(super::DirectoryHandle {
                inner: DirectoryHandle {
                    dir: AsyncMutex::new(subdir),
                },
            })))
        } else {
            let mut open_options = OpenOptions::new();

            open_options.read(true);
            // In the event of a symlink, we don't want to follow it and we want to open it as a symlink. This is different
            // on different OSes. Since this function is only ever given a single path component and we use `openat`, we
            // don't have to worry about symlinks in the containing directories, but we do need to handle opening a symlink
            // directly.
            cfg_if::cfg_if! {
                if #[cfg(target_os = "linux")] {
                    // Linux only allows opening symlinks with O_PATH | O_NOFOLLOW, but if this is a regular file an O_PATH
                    // descriptor will be insufficient. So we open with just O_NOFOLLOW and retry in the event of an ELOOP.
                    // We don't have to worry about ELOOP being ambiguous since we only open direct children of the given
                    // directory file descriptor.
                    open_options.nofollow(true);
                } else if #[cfg(target_os = "macos")] {
                    // macOS has an O_SYMLINK flag for this purpose
                    // TODO: I have PRs in progress to add this to libc, then nix. Use those once that is done.
                    open_options.open_symlink(true);
                } else {
                    compile_error!("unsupported platform");
                }
            }

            let mut file = {
                let mut dir = self.dir.lock().await;
                match dir.open_at_file_with_options(&open_options, path_segment).await {
                    Ok(file) => file,
                    Err(ref err) if err.raw_os_error() == Some(libc::ENOENT) => return Err(types::Errno::Noent),
                    Err(ref err) if err.raw_os_error() == Some(libc::ENOTDIR) => return Err(types::Errno::Notdir),
                    #[cfg(target_os = "linux")]
                    Err(ref err) if err.raw_os_error() == Some(libc::ELOOP) => {
                        // The file in question is a symlink, we need to open it with O_PATH | O_NOFOLLOW
                        todo!()
                    }
                    Err(_error) => {
                        return Err(types::Errno::Io);
                    }
                }
            };

            // Check filename is an exact match for the requested name
            #[cfg(target_os = "macos")]
            {
                use cealn_core::fs::unix::path_of_fd;
                use compio_fs::os::macos::FileExt;

                let file_path = path_of_fd(file.as_raw_fd()).map_err(|_| types::Errno::Io)?;
                // Do comparison at byte level so we don't have to waste time with a utf-8 check
                if file_path.file_name().map(|x| x.as_bytes()) != Some(path_segment.as_bytes()) {
                    error!(code = "open_file_different_case");
                    return Err(types::Errno::Noent);
                }
            }

            let metadata = file.symlink_metadata().await.map_err(|_| types::Errno::Io)?;

            if metadata.is_file() {
                Ok(super::Handle::RegularFile(Arc::new(super::RegularFileHandle {
                    inner: RegularFileHandle {
                        file: AsyncMutex::new(file),
                    },
                })))
            } else if metadata.is_symlink() {
                todo!()
            } else {
                // Unsupported file type, pretend it doesn't exist
                return Err(types::Errno::Noent);
            }
        }
    }

    pub(super) async fn readdir<'a>(
        &'a self,
        cookie: types::Dircookie,
    ) -> Result<Box<dyn Iterator<Item = Result<(types::Dirent, String)>> + 'a>> {
        let mut entries = Vec::new();
        {
            let mut dir = self.dir.lock().await;
            let mut readdir = dir.read_dir().await.map_err(|_| types::Errno::Io)?;
            while let Some(entry) = readdir.try_next().await.map_err(|_| types::Errno::Io)? {
                let file_type = entry.file_type().await.map_err(|_| types::Errno::Io)?;
                let file_type = if file_type.is_file() {
                    types::Filetype::RegularFile
                } else if file_type.is_dir() {
                    types::Filetype::Directory
                } else if file_type.is_symlink() {
                    types::Filetype::SymbolicLink
                } else {
                    // Unsupported file type, pretend it doesn't exist
                    continue;
                };

                let filename = entry.file_name().into_owned();
                let Ok(filename) = filename.into_string() else {
                    // Invalid UTF-8 in filename, pretend it doesn't exist
                    continue;
                };

                let dirent = types::Dirent {
                    // We can't set the cookies until we've sorted the entries
                    d_next: 0,
                    // FIXME: find unique but deterministic values for ino
                    d_ino: 0,
                    d_namlen: u32::try_from(entry.file_name().len()).map_err(|_| types::Errno::Overflow)?,
                    d_type: file_type,
                };
                entries.push((dirent, filename));
            }
        }

        entries.sort_by(|(_, a), (_, b)| a.cmp(b));

        // Fill in next directory cookies
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.0.d_next = (index + 1) as u64;
        }

        // Return appropriately sliced iterator
        let iterator = entries.into_iter().skip(cookie as usize).map(Ok);
        Ok(Box::new(iterator))
    }

    pub(super) async fn readlinkat_child(&self, path_segment: &str) -> Result<String> {
        let mut dir = self.dir.lock().await;
        match dir.read_link_at(path_segment).await {
            Ok(target) => {
                let Ok(target) = target.into_os_string().into_string() else {
                    return Err(types::Errno::Io);
                };
                Ok(target)
            }
            Err(ref err) if err.raw_os_error() == Some(libc::ENOENT) => Err(types::Errno::Noent),
            // (Hopefully) indicates that this file is not a symlink
            Err(ref err) if err.raw_os_error() == Some(libc::EINVAL) => Err(types::Errno::Inval),
            Err(err) => {
                error!("uncacheable file error when executing 'readlinkat': {}", err);
                Err(types::Errno::Io)
            }
        }
    }

    pub(super) async fn filestat(&self) -> Result<types::Filestat> {
        todo!()
    }

    pub(super) async fn filestat_child(&self, path_segment: &str) -> Result<types::Filestat> {
        let mut dir = self.dir.lock().await;
        match dir.symlink_metadata_at(path_segment).await {
            Ok(metadata) => filestat_convert(&metadata).ok_or_else(|| {
                // Unsupported file type, pretend it doesn't exist
                types::Errno::Noent
            }),
            Err(ref err) if err.raw_os_error() == Some(libc::ENOENT) => Err(types::Errno::Noent),
            Err(error) => {
                error!("uncacheable file error when executing 'fstat': {}", error);
                Err(types::Errno::Io)
            }
        }
    }
}

fn filestat_convert(data: &Metadata) -> Option<types::Filestat> {
    let filestat = types::Filestat {
        // FIXME: find unique but deterministic values for these
        dev: 0,
        ino: 0,
        filetype: {
            if data.is_file() {
                types::Filetype::RegularFile
            } else if data.is_dir() {
                types::Filetype::Directory
            } else if data.is_symlink() {
                types::Filetype::SymbolicLink
            } else {
                // Unsupported file type, pretend it doesn't exist
                return None;
            }
        },
        // Hard link count is not part of file cache key so ignore them
        nlink: 1,
        size: data.len(),
        // Ignore timestamps for determinism
        atim: 0,
        mtim: 0,
        ctim: 0,
    };
    Some(filestat)
}

impl RegularFileHandle {
    pub(super) async fn read<'a>(&self, iovs: &mut [io::IoSliceMut<'a>]) -> Result<usize> {
        let mut file = self.file.lock().await;
        file.read(UnpinnedIoVecMut(iovs)).await.map_err(|_| types::Errno::Io)
    }

    pub(super) async fn tell(&self) -> Result<types::Filesize> {
        self.seek(SeekFrom::Current(0)).await
    }

    pub(super) async fn seek(&self, pos: SeekFrom) -> Result<u64> {
        let mut file = self.file.lock().await;
        file.seek(pos).await.map_err(|_| types::Errno::Io)
    }

    pub(super) async fn filestat(&self) -> Result<types::Filestat> {
        let mut file = self.file.lock().await;
        let metadata = file.symlink_metadata().await.map_err(|_| types::Errno::Io)?;
        Ok(filestat_convert(&metadata).unwrap())
    }
}

impl fmt::Debug for DirectoryHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("system::DirectoryHandle").finish()
    }
}

impl fmt::Debug for RegularFileHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("system::RegularFileHandle").finish()
    }
}
