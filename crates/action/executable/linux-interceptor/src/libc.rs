use core::ffi::{c_char, c_int};

use libc::{gid_t, uid_t};
use syscalls::{raw_syscall, Sysno};

// #[no_mangle]
// pub unsafe extern "C" fn open(path: *const c_char, oflag: c_int, mut args: ...) -> c_int {
//     let mode = if oflag & libc::O_CREAT != 0 || oflag & libc::O_TMPFILE == libc::O_TMPFILE {
//         args.arg::<c_int>()
//     } else {
//         0
//     };
//     do_openat(libc::AT_FDCWD, path, oflag, mode)
// }

// #[no_mangle]
// pub unsafe extern "C" fn openat(dirfd: c_int, path: *const c_char, oflag: c_int, mut args: ...) -> c_int {
//     let mode = if oflag & libc::O_CREAT != 0 || oflag & libc::O_TMPFILE == libc::O_TMPFILE {
//         args.arg::<c_int>()
//     } else {
//         0
//     };
//     do_openat(dirfd, path, oflag, mode)
// }

// unsafe fn do_openat(dirfd: c_int, path: *const c_char, oflag: c_int, mode: c_int) -> c_int {
//     libc_syscall_ret(raw_syscall!(Sysno::openat, dirfd, path, oflag, mode))
// }

#[no_mangle]
pub unsafe extern "C" fn chown(_path: *const c_char, _owner: uid_t, _group: gid_t) -> c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn fchown(_fd: c_int, _owner: uid_t, _group: gid_t) -> c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn lchown(_path: *const c_char, _owner: uid_t, _group: gid_t) -> c_int {
    0
}

#[no_mangle]
pub unsafe extern "C" fn fchownat(
    _dirfd: c_int,
    _path: *const c_char,
    _owner: uid_t,
    _group: gid_t,
    _flags: c_int,
) -> c_int {
    0
}

#[inline(always)]
unsafe fn libc_syscall_ret(ret: usize) -> c_int {
    let ret_signed = ret as isize;
    if ret_signed < 0 && ret_signed > -1024 {
        *libc::__errno_location() = -ret_signed as c_int - 1;
        -1
    } else {
        ret_signed as c_int
    }
}
