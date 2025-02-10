#![feature(thread_local)]
#![feature(vec_into_raw_parts)]

// Injected functions only needed with wasi's libc
cfg_if::cfg_if! {
    if #[cfg(target_os = "wasi")] {
        mod libc_ext;
    }
}

#[macro_use]
mod abi;

mod error;
mod package;
mod python;
mod rule;
mod workspace;

use error::Error;

pub fn init_patches() {
    // This doesn't do anything but it ensures that our implicit functions get linked in
}
