use std::{
    arch::asm,
    ffi::c_uint,
    os::raw::{c_char, c_int, c_long, c_ulong, c_void},
};

use libc::size_t;
use syscalls::Errno;

pub(crate) unsafe fn pivot_root(new_root: *const c_char, put_old: *const c_char) -> c_long {
    libc::syscall(libc::SYS_pivot_root, new_root, put_old)
}

#[inline(always)]
pub(crate) unsafe fn clone3(args: *mut libc::clone_args, size: size_t) -> Result<libc::pid_t, Errno> {
    cfg_if::cfg_if! {
        if #[cfg(target_arch = "x86_64")] {
            let mut ret: isize;
            asm!(
                "syscall",
                inlateout("rax") libc::SYS_clone3 as usize => ret,
                in("rdi") args,
                in("rsi") size,
                out("rcx") _,
                out("r11") _,
                options(nostack, preserves_flags),
            );
            if ret >= 0 {
                Ok(ret as libc::pid_t)
            } else {
                Err(Errno::new((-ret) as c_int))
            }
        } else if #[cfg(target_arch = "aarch64")] {
            let mut ret: isize;
            asm!(
                "svc #0",
                in("x8") libc::SYS_clone3 as usize,
                inlateout("x0") args => ret,
                in("x1") size,
                out("x2") _,
                out("x3") _,
                out("x4") _,
                out("x5") _,
                out("x6") _,
                out("x7") _,
                options(nostack, preserves_flags),
            );
            if ret >= 0 {
                Ok(ret as libc::pid_t)
            } else {
                Err(Errno::new((-ret) as c_int))
            }
        } else {
            compile_error!("unsupported architecture");
        }
    }
}

pub(crate) unsafe fn pidfd_open(pid: libc::pid_t, flags: c_uint) -> c_long {
    libc::syscall(libc::SYS_pidfd_open, pid, flags)
}

pub(crate) unsafe fn pidfd_send_signal(
    pidfd: c_int,
    sig: c_int,
    siginfo: *mut libc::siginfo_t,
    flags: c_uint,
) -> c_long {
    libc::syscall(libc::SYS_pidfd_send_signal, pidfd, sig, siginfo, flags)
}
