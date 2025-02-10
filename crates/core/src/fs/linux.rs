pub(crate) use super::unix::FileNodeIdentifier;

use std::{
    ffi::{CString, OsString},
    io,
    os::unix::prelude::*,
    path::PathBuf,
};

use libc::c_char;

use crate::libc_call;

pub fn path_of_fd(fd: RawFd) -> io::Result<PathBuf> {
    let mut buffer = [0u8; libc::PATH_MAX as usize];

    let procfs_filename = CString::new(format!("/proc/self/fd/{}", fd)).unwrap();

    // TODO: not sure this works when files have been moved
    let path_len = unsafe {
        libc_call!(libc::readlink(
            procfs_filename.as_ptr(),
            buffer.as_mut_ptr() as *mut c_char,
            buffer.len()
        ))?
    } as usize;

    let path_buf = PathBuf::from(OsString::from_vec(buffer[..path_len].to_vec()));

    // Filter out things like `socket:1234`
    if !path_buf.starts_with("/") {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "file descriptor not associated with a file path",
        ));
    }

    Ok(path_buf)
}
