#![feature(thread_local)]

#[cfg(feature = "local-pool")]
mod local_pool;

#[cfg(feature = "thread-pool")]
pub mod thread_pool;

mod sleep;
pub mod spawn;

pub use self::{
    sleep::{sleep_until, Sleep},
    spawn::{block, spawn, spawn_blocking, spawn_blocking_handle, spawn_handle},
};

#[cfg(feature = "local-pool")]
pub use self::local_pool::{LocalPool, LocalSpawner};

#[cfg(feature = "thread-pool")]
pub use self::thread_pool::ThreadPool;
