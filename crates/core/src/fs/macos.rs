pub(crate) use super::unix::FileNodeIdentifier;

use std::{ffi::OsString, fs::File, io, os::unix::prelude::*, path::PathBuf};

use tracing::{error, trace_span};

use super::FileExt;
use crate::tracing::error_value;

const MAXPATHLEN: usize = 1024;

pub fn path_of_fd(fd: RawFd) -> io::Result<PathBuf> {
    let mut buffer = [0u8; MAXPATHLEN];

    let fcntl_span = trace_span!("fcntl", ?fd, cmd = "F_GETPATH");
    {
        let _guard = fcntl_span.enter();

        unsafe {
            if libc::fcntl(fd, libc::F_GETPATH, buffer.as_mut_ptr()) < 0 {
                let err = io::Error::last_os_error();
                error!(error = error_value(&err));
                return Err(err);
            }
        }
    }

    let path_len = buffer.iter().position(|&x| x == 0).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "missing null terminator in filename obtained from fcntl(F_GETPATH)",
        )
    })?;

    Ok(PathBuf::from(OsString::from_vec(buffer[..path_len].to_vec())))
}
