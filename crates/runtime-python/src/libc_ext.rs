#![allow(non_camel_case_types)]

use std::{ffi::CStr, ptr};

use libc::{c_char, c_int, c_void, size_t};

type mode_t = c_int;

type sighandler_t = Option<extern "C" fn(arg: c_int)>;

#[no_mangle]
extern "C" fn __libc_current_sigrtmin() -> c_int {
    unimplemented!()
}

#[no_mangle]
extern "C" fn __libc_current_sigrtmax() -> c_int {
    unimplemented!()
}

#[no_mangle]
extern "C" fn raise(_sig: c_int) -> c_int {
    unimplemented!()
}

#[no_mangle]
extern "C" fn signal(_signum: c_int, _handler: sighandler_t) -> sighandler_t {
    unimplemented!("{}", _signum)
}

#[no_mangle]
unsafe extern "C" fn getcwd(buf: *mut c_char, size: size_t) -> *mut c_char {
    let root = CStr::from_bytes_with_nul(b"/\0").unwrap();
    if size >= root.to_bytes_with_nul().len() {
        ptr::copy_nonoverlapping(
            root.to_bytes_with_nul().as_ptr() as *const c_char,
            buf,
            root.to_bytes_with_nul().len(),
        );
        buf
    } else {
        errno = libc::ERANGE;
        return ptr::null_mut();
    }
}

#[no_mangle]
unsafe extern "C" fn chdir(_path: *const c_char) -> c_int {
    // TODO: implement
    errno = libc::EACCES;
    -1
}

#[no_mangle]
extern "C" fn umask(_mode: mode_t) -> mode_t {
    unimplemented!()
}

#[no_mangle]
unsafe extern "C" fn dup(old_fd: c_int) -> c_int {
    let ret = dup_hack(old_fd);
    if ret < 0 {
        errno = -ret;
        -1
    } else {
        ret
    }
}

#[no_mangle]
unsafe extern "C" fn times(times: *mut c_void) -> libc::clock_t {
    todo!()
}

#[no_mangle]
unsafe extern "C" fn getrusage(who: c_int, usage: *mut libc::rusage) -> c_int {
    todo!()
}

#[no_mangle]
unsafe extern "C" fn clock() -> libc::clock_t {
    todo!()
}

#[no_mangle]
unsafe extern "C" fn getpid() -> libc::c_int {
    2
}

#[no_mangle]
unsafe extern "C" fn strsignal(sig: libc::c_int) -> *mut libc::c_char {
    todo!()
}

extern "C" {
    #[thread_local]
    #[link_name = "errno"]
    static mut errno: c_int;

    fn dup_hack(fd: c_int) -> c_int;
}
