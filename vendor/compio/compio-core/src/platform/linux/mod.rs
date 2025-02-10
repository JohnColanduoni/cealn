pub mod ext;
pub(crate) mod poller;

use std::io;

use crate::{
    epoll::event_queue::Epoll,
    event_queue::{EventQueueFactory, EventQueueImpl},
};

#[cfg(feature = "io-uring")]
#[repr(usize)]
enum IoUringSupport {
    Unknown = 0,
    Supported = 1,
    Unsupported = 2,
}

#[cfg(feature = "io-uring")]
static IO_URING_SUPPORT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(IoUringSupport::Unknown as usize);

pub(crate) fn default_event_queue() -> io::Result<Box<dyn EventQueueImpl>> {
    #[cfg(feature = "io-uring")]
    {
        use crate::io_uring::IoUring;
        use libc::ENOSYS;
        use std::sync::atomic::Ordering;

        if IO_URING_SUPPORT.load(Ordering::Relaxed) != IoUringSupport::Unsupported as usize {
            // Prefer io_uring
            match IoUring::new(Default::default()) {
                Ok(ring) => return Ok(Box::new(ring)),
                Err(ref err) if err.raw_os_error() == Some(ENOSYS) => {
                    IO_URING_SUPPORT.store(IoUringSupport::Unsupported as usize, Ordering::Relaxed);
                    tracing::warn!("io_uring not supported: {}", err);
                    // io_uring not supported, fallback on epoll
                }
                Err(err) => return Err(err),
            }
        }
    }
    let epoll = Epoll::new()?;
    Ok(Box::new(epoll))
}

pub(crate) fn default_event_queue_factory() -> io::Result<Box<dyn EventQueueFactory>> {
    #[cfg(feature = "io-uring")]
    {
        use crate::io_uring::IoUring;
        use libc::ENOSYS;
        use std::sync::atomic::Ordering;

        match IO_URING_SUPPORT.load(Ordering::Relaxed) {
            code if code == IoUringSupport::Supported as usize => {
                return Ok(Box::new(crate::io_uring::Options::default()));
            }
            code if code == IoUringSupport::Unsupported as usize => todo!(),
            _ => {
                // Prefer io_uring
                match IoUring::new(Default::default()) {
                    Ok(ring) => {
                        IO_URING_SUPPORT.store(IoUringSupport::Supported as usize, Ordering::Relaxed);
                        return Ok(Box::new(crate::io_uring::Options::default()));
                    }
                    Err(ref err) if err.raw_os_error() == Some(ENOSYS) => {
                        IO_URING_SUPPORT.store(IoUringSupport::Unsupported as usize, Ordering::Relaxed);
                        tracing::warn!("io_uring not supported: {}", err);
                    }
                    Err(err) => return Err(err),
                }
            }
        }
    }

    todo!()
}
