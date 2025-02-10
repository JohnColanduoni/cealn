pub use super::sys::path_of_fd;

use std::{
    fs::{File, Metadata},
    io,
    os::unix::{fs::MetadataExt as UnixMetadataExt, prelude::*},
    path::PathBuf,
};

use super::{FileExt, MetadataExt};
use crate::libc_call;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FileNodeIdentifier {
    dev: u64,
    ino: u64,
}

impl FileExt for File {
    fn path(&self) -> io::Result<PathBuf> {
        let fd = self.as_raw_fd();
        path_of_fd(fd)
    }

    fn file_node_identifier(&self) -> io::Result<super::FileNodeIdentifier> {
        self.metadata()?.file_node_identifier()
    }
}

impl MetadataExt for Metadata {
    fn file_node_identifier(&self) -> io::Result<super::FileNodeIdentifier> {
        Ok(super::FileNodeIdentifier {
            inner: FileNodeIdentifier {
                dev: self.dev(),
                ino: self.ino(),
            },
        })
    }
}

#[cfg(feature = "compio-fs")]
impl MetadataExt for compio_fs::Metadata {
    fn file_node_identifier(&self) -> io::Result<super::FileNodeIdentifier> {
        use compio_fs::os::unix::MetadataExt;

        Ok(super::FileNodeIdentifier {
            inner: FileNodeIdentifier {
                dev: self.dev(),
                ino: self.ino(),
            },
        })
    }
}

pub fn set_cloexec(fd: RawFd) -> io::Result<()> {
    let mut flags = unsafe { libc_call!(libc::fcntl(fd, libc::F_GETFD))? };
    flags |= libc::FD_CLOEXEC;
    unsafe { libc_call!(libc::fcntl(fd, libc::F_SETFD, flags))? };
    Ok(())
}
