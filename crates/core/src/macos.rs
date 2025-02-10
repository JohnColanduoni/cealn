use std::{ffi::CStr, io};

use libc::c_char;
use mach::kern_return::kern_return_t;

#[macro_export]
macro_rules! mach_call {
    ( $i:ident $( :: $sub_i:ident )* ( $( $arg:expr ),* $(,)? ) ) => {
        {
            const FUNCTION_NAME: &'static str = concat!(stringify!($i), $("::", stringify!($sub_i), )* );
            let span = ::tracing::trace_span!(FUNCTION_NAME);
            let _guard = span.enter();
            match $i$(::$sub_i)* ( $( $arg, )* ) {
                ret if ret == ::mach::kern_return::KERN_SUCCESS => Ok(()),
                ret => {
                    let err = ::cealn_core::macos::mach_kern_return_to_io_err(ret);
                    let err_ref: &(dyn ::std::error::Error + 'static) = &err;
                    ::tracing::error!(error = err_ref, "{} failed: {}", FUNCTION_NAME, err);
                    Err(err)
                },
            }
        }
    };
}

pub fn mach_kern_return_to_io_err(ret: kern_return_t) -> io::Error {
    unsafe {
        let string_ptr = mach_error_string(ret);
        if !string_ptr.is_null() {
            let string_cstr = CStr::from_ptr(string_ptr);
            if let Ok(string) = string_cstr.to_str() {
                return io::Error::new(io::ErrorKind::Other, format!("mach error({:#x}): {}", ret, string));
            }
        }

        return io::Error::new(io::ErrorKind::Other, format!("mach error({:#x})", ret));
    }
}

#[link(name = "System.B")]
extern "C" {
    fn mach_error_string(ret: kern_return_t) -> *const c_char;
}
