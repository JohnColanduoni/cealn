#![feature(maybe_uninit_uninit_array)]
#![feature(io_error_more)]

mod entry;
mod reference;
mod source_monitor;
mod watcher;

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

pub use self::{
    entry::{DirectoryStatus, FileStatus, Status},
    reference::SourceReference,
    source_monitor::SourceMonitor,
};
