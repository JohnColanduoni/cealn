pub mod message_channel;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        #[path = "platform/linux/mod.rs"]
        mod platform;

        pub mod os {
            pub mod windows {
                pub use crate::platform::message_channel::MessageChannelExt;
            }
        }
    } else if #[cfg(target_os = "windows")] {
        #[path = "platform/windows/mod.rs"]
        mod platform;

        pub mod os {
            pub mod windows {
                pub use crate::platform::ext::*;
            }
        }
    }  else if #[cfg(target_os = "macos")] {
        #[path = "platform/macos/mod.rs"]
        mod platform;

        pub mod os {
            pub mod macos {
                pub use crate::platform::ext::*;
            }
        }
    } else {
        compile_error!("unsupported platform");
    }
}

pub use self::message_channel::MessageChannel;
