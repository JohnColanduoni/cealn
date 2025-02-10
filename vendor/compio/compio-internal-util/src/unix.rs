use std::{io, mem, os::unix::prelude::*};

#[macro_export]
macro_rules! libc_fd_call {
    { libc :: $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        {
            let span = ::tracing::span!(::tracing::Level::TRACE, ::std::stringify!($func_name));
            let _guard = span.enter();
            let fd: ::std::os::unix::io::RawFd = libc :: $func_name ( $($arg,)* );
            if fd < 0 {
                let err = ::std::io::Error::last_os_error();
                ::tracing::error!(
                    what = "libc_error",
                    function = ::std::stringify!($func_name),
                    error_code = ?err.raw_os_error(),
                    "function {} failed: {}", ::std::stringify!($func_name), err);
                Err(err)
            } else {
                Ok(::compio_internal_util::unix::ScopedFd::from_raw(fd))
            }
        }
    };
}

#[macro_export]
macro_rules! libc_call {
    { libc :: $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        {
            let span = ::tracing::span!(::tracing::Level::TRACE, ::std::stringify!($func_name));
            let _guard = span.enter();
            let ret = libc :: $func_name ( $($arg,)* );
            if ret < 0 {
                let err = ::std::io::Error::last_os_error();
                ::tracing::error!(
                    what = "libc_error",
                    function = ::std::stringify!($func_name),
                    error_code = ?err.raw_os_error(),
                    "function {} failed: {}", ::std::stringify!($func_name), err);
                Err(err)
            } else {
                Ok(ret)
            }
        }
    };
}

#[derive(Debug)]
pub struct ScopedFd(RawFd);

impl Drop for ScopedFd {
    #[inline]
    fn drop(&mut self) {
        unsafe {
            libc::close(self.0);
        }
    }
}

impl ScopedFd {
    #[inline]
    pub unsafe fn from_raw(fd: RawFd) -> Self {
        ScopedFd(fd)
    }

    #[inline]
    pub fn as_raw(&self) -> RawFd {
        self.0
    }

    #[inline]
    pub unsafe fn into_raw_fd(self) -> RawFd {
        let fd = self.0;
        mem::forget(self);
        fd
    }
}

impl FromRawFd for ScopedFd {
    #[inline]
    unsafe fn from_raw_fd(fd: RawFd) -> Self {
        ScopedFd(fd)
    }
}

impl AsRawFd for ScopedFd {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.0
    }
}

#[inline]
pub unsafe fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let mut flags = libc_call!(libc::fcntl(fd, libc::F_GETFL))?;
    flags |= libc::O_NONBLOCK;
    libc_call!(libc::fcntl(fd, libc::F_SETFL, flags))?;
    Ok(())
}
