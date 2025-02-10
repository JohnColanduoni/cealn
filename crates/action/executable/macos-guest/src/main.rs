#![feature(lang_items, naked_functions)]
#![feature(asm_const)]
#![feature(maybe_uninit_uninit_array)]
#![feature(panic_info_message)]
#![no_std]
#![no_main]

mod util;

cfg_if::cfg_if! {
    if #[cfg(target_arch = "aarch64")] {
        #[path = "arch/aarch64.rs"]
        mod arch;
    } else {
        compile_error!("unsupported architecture");
    }
}

use core::mem::MaybeUninit;

fn main() -> ! {
    arch::main_hypervisor_exit()
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    unsafe {
        let formatted = util::format_static(&mut PANIC_BUFFER[..], format_args!("{}", info.message()));
        arch::panic_exit(formatted);
    }
}

static mut PANIC_BUFFER: [u8; 64 * 1024] = [0u8; 64 * 1024];

pub const KERNEL_STACK_SIZE: usize = 1 * 1024 * 1024;

pub static mut BOOTSTRAP_KERNEL_STACK: [MaybeUninit<u8>; KERNEL_STACK_SIZE] =
    MaybeUninit::uninit_array::<KERNEL_STACK_SIZE>();
