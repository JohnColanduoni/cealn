#[macro_export]
macro_rules! winapi_handle_call {
    { @bad_handle: $bad_handle:expr => $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        {
            let span = ::tracing::span!(::tracing::Level::TRACE, ::std::stringify!($func_name));
            let _guard = span.enter();
            let raw_handle = $func_name ( $($arg,)* );
            if raw_handle == $bad_handle {
                let err = ::std::io::Error::last_os_error();
                ::tracing::error!(
                    what = "win32_error",
                    function = ::std::stringify!($func_name),
                    error_code = ?err.raw_os_error(),
                    "function {} failed: {}", ::std::stringify!($func_name), err);
                Err(err)
            } else {
                Ok(::winhandle::WinHandle::from_raw_unchecked(raw_handle))
            }
        }
    };
    { $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        winapi_handle_call!(@bad_handle: ::winapi::um::handleapi::INVALID_HANDLE_VALUE => $func_name ( $( $arg, )* ))
    };
    { null_on_error: $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        winapi_handle_call!(@bad_handle: ::std::ptr::null_mut() => $func_name ( $( $arg, )* ))
    };
}

#[macro_export]
macro_rules! winapi_bool_call {
    { @error_trace: $error_trace:ident => $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        {
            let span = ::tracing::span!(::tracing::Level::TRACE, ::std::stringify!($func_name));
            let _guard = span.enter();
            let raw_handle: ::winapi::shared::minwindef::BOOL = $func_name ( $($arg,)* );
            if raw_handle == ::winapi::shared::minwindef::FALSE {
                let err = ::std::io::Error::last_os_error();
                ::tracing::$error_trace!(
                    what = "win32_error",
                    function = ::std::stringify!($func_name),
                    error_code = ?err.raw_os_error(),
                    "function {} failed: {}", ::std::stringify!($func_name), err);
                Err(err)
            } else {
                Ok(())
            }
        }
    };
    { io_pending_ok: $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        {
            let span = ::tracing::span!(::tracing::Level::TRACE, ::std::stringify!($func_name));
            let _guard = span.enter();
            let raw_handle: ::winapi::shared::minwindef::BOOL = $func_name ( $($arg,)* );
            if raw_handle == ::winapi::shared::minwindef::FALSE {
                let err = ::std::io::Error::last_os_error();
                if err.raw_os_error() == Some(::winapi::shared::winerror::ERROR_IO_PENDING as i32) {
                    ::tracing::trace!(
                        what = "win32_error",
                        function = ::std::stringify!($func_name),
                        error_code = ?err.raw_os_error(),
                        "function {} failed: {}", ::std::stringify!($func_name), err);
                } else {
                    ::tracing::error!(
                        what = "win32_error",
                        function = ::std::stringify!($func_name),
                        error_code = ?err.raw_os_error(),
                        "function {} failed: {}", ::std::stringify!($func_name), err);
                }
                Err(err)
            } else {
                Ok(())
            }
        }
    };
    { warn: $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        $crate::winapi_bool_call!( @error_trace:warn => $func_name ( $($arg,)* ) )
    };
    { no_error: $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        $crate::winapi_bool_call!( @error_trace:debug => $func_name ( $($arg,)* ) )
    };
    { $func_name:ident ( $($arg:expr),* $(,)* ) } => {
        $crate::winapi_bool_call!( @error_trace:error => $func_name ( $($arg,)* ) )
    };
}
