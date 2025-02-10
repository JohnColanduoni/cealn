#![feature(try_blocks)]
#![feature(maybe_uninit_uninit_array)]
#![feature(maybe_uninit_slice)]
#![feature(type_alias_impl_trait, impl_trait_in_assoc_type)]

mod directory;
mod file;
mod metadata;
mod open_options;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        #[path = "platform/linux.rs"]
        mod platform;

        #[path = "platform/unix.rs"]
        mod platform_unix;

        pub mod os {
            pub mod linux {
                pub use crate::platform::{OpenOptionsExt, FileExt, MetadataExt};
            }

            pub mod unix {
                pub use crate::platform_unix::*;
            }
        }
    } else if #[cfg(target_os = "macos")] {
        #[path = "platform/macos.rs"]
        mod platform;

        #[path = "platform/unix.rs"]
        mod platform_unix;

        pub mod os {
            pub mod macos {
                pub use crate::platform::{OpenOptionsExt, FileExt};
            }

            pub mod unix {
                pub use crate::platform_unix::*;
            }
        }
    } else {
        compile_error!("unsupported platform");
    }
}

pub use crate::{
    directory::{remove_dir_all, DirEntry, Directory, ReadDir},
    file::{read, remove_file, rename, symlink_metadata, File},
    metadata::{Metadata, Permissions},
    open_options::OpenOptions,
};
