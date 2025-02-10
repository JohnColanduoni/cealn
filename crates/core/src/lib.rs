#![cfg_attr(target_os = "windows", feature(windows_by_handle))]
#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
pub mod files;
#[cfg(feature = "std")]
pub mod fs;
pub mod ptr;
pub mod tracing;

#[cfg(all(target_os = "macos", feature = "std"))]
pub mod macos;

#[cfg(unix)]
pub mod libc;
