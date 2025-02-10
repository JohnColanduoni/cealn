use std::io;

use crate::epoll::event_queue::Epoll;

pub trait EventQueueExt: Sized {
    fn epoll() -> io::Result<Self>;

    #[cfg(feature = "io-uring")]
    fn io_uring(options: crate::io_uring::Options) -> io::Result<Self>;

    fn kind(&self) -> EventQueueKind;

    fn kind_mut(&mut self) -> EventQueueKindMut;
}

pub(crate) trait EventQueueImplExt {
    fn kind(&self) -> EventQueueKind;

    fn kind_mut(&mut self) -> EventQueueKindMut;
}

#[non_exhaustive]
pub enum EventQueueKind<'a> {
    Epoll(&'a Epoll),
    #[cfg(feature = "io-uring")]
    IoUring(&'a crate::io_uring::IoUring),
}

#[non_exhaustive]
pub enum EventQueueKindMut<'a> {
    Epoll(&'a mut Epoll),
    #[cfg(feature = "io-uring")]
    IoUring(&'a mut crate::io_uring::IoUring),
}

impl EventQueueExt for crate::EventQueue {
    fn epoll() -> io::Result<Self> {
        Ok(Self::with_imp(Box::new(Epoll::new()?)))
    }

    #[cfg(feature = "io-uring")]
    fn io_uring(options: crate::io_uring::Options) -> io::Result<Self> {
        Ok(Self::with_imp(Box::new(crate::io_uring::IoUring::new(options)?)))
    }

    #[inline]
    fn kind(&self) -> EventQueueKind {
        self.imp.kind()
    }

    #[inline]
    fn kind_mut(&mut self) -> EventQueueKindMut {
        self.imp.kind_mut()
    }
}
