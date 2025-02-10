use std::{
    ffi::OsString,
    fs::{File, Metadata},
    io,
    os::windows::{
        fs::MetadataExt as WindowsMetadataExt,
        prelude::{AsRawHandle, OsStringExt},
    },
    path::PathBuf,
};

use winapi::{
    shared::minwindef::{DWORD, MAX_PATH},
    um::fileapi::GetFinalPathNameByHandleW,
};

use super::{FileExt, MetadataExt};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct FileNodeIdentifier {
    volume_serial_number: u32,
    file_index: u64,
}

impl FileExt for File {
    fn path(&self) -> io::Result<PathBuf> {
        // NOTE: we use MAX_PATH as our initial buffer capacity, but we handle longer paths
        let mut buffer = vec![0u16; MAX_PATH];

        loop {
            let received_len = unsafe {
                GetFinalPathNameByHandleW(self.as_raw_handle(), buffer.as_mut_ptr(), buffer.len() as DWORD, 0)
            };
            // If the buffer was big enough, the return value is the length of the string without the null terminator. If it's too large, it's the
            // length of the actual filename *with* the null terminator. So a length >= `buffer.len()` indicates a failure.
            if received_len as usize >= buffer.len() {
                buffer.resize(received_len as usize, 0);
                continue;
            } else if received_len < 1 {
                return Err(io::Error::last_os_error());
            } else {
                // `received_len` doesn't include null terminator
                buffer.truncate(received_len as usize);
                return Ok(PathBuf::from(OsString::from_wide(&buffer)));
            }
        }
    }

    fn file_node_identifier(&self) -> io::Result<super::FileNodeIdentifier> {
        self.metadata()?.file_node_identifier()
    }
}

impl MetadataExt for Metadata {
    fn file_node_identifier(&self) -> io::Result<super::FileNodeIdentifier> {
        Ok(super::FileNodeIdentifier {
            inner: FileNodeIdentifier {
                volume_serial_number: self
                    .volume_serial_number()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "file node identifier not fetched"))?,
                file_index: self
                    .file_index()
                    .ok_or_else(|| io::Error::new(io::ErrorKind::Other, "file node identifier not fetched"))?,
            },
        })
    }
}
