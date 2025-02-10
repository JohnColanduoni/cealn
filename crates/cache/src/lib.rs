mod action;
mod depmap;
pub mod fs;
mod hashing;
pub mod hot_disk;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        #[path = "platform/linux.rs"]
        mod platform;
    } else if #[cfg(target_os = "macos")] {
        #[path = "platform/macos.rs"]
        mod platform;
    } else {
        compile_error!("unsupported platform");
    }
}

pub use self::{hashing::hash_serializable, hot_disk::HotDiskCache};
