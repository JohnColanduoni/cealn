use std::io;

use mach::kern_return::kern_return_t;

macro_rules! mach_call {
    { $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        {
            let span = ::tracing::span!(::tracing::Level::TRACE, ::std::stringify!($func_name));
            let _guard = span.enter();
            let ret = $func_name ( $($arg,)* );
            if ret != 0 {
                let err = $crate::platform::mach::kern_return_err(ret);
                ::tracing::error!(
                    what = "mach_error",
                    function = ::std::stringify!($func_name),
                    error_code = ret,
                    "function {} failed: {}", ::std::stringify!($func_name), err);
                Err(err)
            } else {
                Ok(())
            }
        }
    };
}

pub(crate) fn kern_return_err(ret: kern_return_t) -> io::Error {
    // TODO: get error message string
    io::Error::new(io::ErrorKind::Other, format!("mach kernel error {:#x}", ret))
}
