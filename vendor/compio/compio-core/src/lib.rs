#![feature(maybe_uninit_uninit_array, maybe_uninit_slice)]
#![feature(thread_local)]
#![feature(try_blocks)]
#![feature(associated_type_defaults, type_alias_impl_trait)]

pub mod buffer;
pub mod event_queue;
pub mod io;

cfg_if::cfg_if! {
    if #[cfg(target_os = "windows")] {
        #[path = "platform/windows/mod.rs"]
        mod platform;

        pub mod iocp;

        pub mod os {
            pub mod windows {
                pub use crate::platform::ext::*;
            }
        }
    } else if #[cfg(target_os = "linux")] {
        #[path = "platform/linux/mod.rs"]
        mod platform;

        pub mod epoll;

        #[cfg(feature = "io-uring")]
        pub mod io_uring;

        pub mod os {
            pub mod linux {
                pub use crate::platform::ext::*;
                pub use crate::platform::poller::{Poller};
            }

            pub mod unix {
                pub use crate::unix::*;
            }
        }
    } else if #[cfg(target_os = "macos")] {
        #[path = "platform/macos/mod.rs"]
        mod platform;

        pub mod kqueue;

        pub mod os {
            pub mod macos {
                pub use crate::platform::ext::*;
            }

            pub mod unix {
                pub use crate::unix::*;
            }
        }
    } else if #[cfg(target_arch = "wasm32")] {
        #[path = "platform/web/mod.rs"]
        mod platform;
    } else {
        compile_error!("unsupported platform");
    }
}

#[cfg(unix)]
pub mod unix;

pub use crate::event_queue::EventQueue;
