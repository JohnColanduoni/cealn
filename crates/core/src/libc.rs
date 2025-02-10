#[macro_export]
macro_rules! libc_call {
    ( $i:ident $( :: $sub_i:ident )* ( $( $arg:expr ),* $(,)? ) ) => {
        {
            const FUNCTION_NAME: &'static str = {
                // Ignore leading components in path so e.g. `libc::read` maps to `read`
                stringify!($i) $(; stringify!($sub_i) )*
            };
            let span = ::tracing::trace_span!(target: "libc", FUNCTION_NAME);
            let _guard = span.enter();
            match $i$(::$sub_i)* ( $( $arg, )* ) {
                // We only use -1 (instead of < 0) for system calls that may return an address in the top half of the
                // address space
                ret if ret == -1 => {
                    let err = ::std::io::Error::last_os_error();
                    let err_ref: &(dyn ::std::error::Error + 'static) = &err;
                    ::tracing::error!(error = err_ref, "{} failed: {}", FUNCTION_NAME, err);
                    ::std::result::Result::Err(err)
                },
                ret => ::std::result::Result::Ok(ret),
            }
        }
    };
}
