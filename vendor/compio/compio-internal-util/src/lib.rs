pub mod buffer;
mod lazy_arc;

#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(unix)]
pub mod unix;

#[cfg(feature = "test")]
pub mod test;

pub use self::lazy_arc::LazyArc;
