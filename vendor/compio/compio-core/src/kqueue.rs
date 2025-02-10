pub(crate) mod event_queue;
pub(crate) mod registration;

#[cfg(target_os = "macos")]
pub(crate) mod mach_registration;

pub use self::{
    event_queue::{KQueue, Options},
    registration::{Registration, WaitForRead, WaitForWrite},
};

#[cfg(target_os = "macos")]
pub use self::mach_registration::{MachRegistration, WaitForRead as MachWaitForRead};
