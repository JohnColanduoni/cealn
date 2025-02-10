use std::{io, os::unix::prelude::RawFd};

use crate::{os::linux::EventQueueExt, EventQueue};

use super::ext::EventQueueKind;

pub struct Poller(_Poller);

enum _Poller {
    Epoll(crate::epoll::Registration),
    #[cfg(feature = "io-uring")]
    IoUring(crate::io_uring::Poller),
}

impl Poller {
    /// Establishes a [`Poller`] with the currently active event queue
    pub unsafe fn new(fd: RawFd) -> io::Result<Poller> {
        EventQueue::with_current(|event_queue| match event_queue.kind() {
            EventQueueKind::Epoll(event_queue) => {
                let registration = crate::epoll::Registration::register(event_queue, fd)?;
                Ok(Poller(_Poller::Epoll(registration)))
            }
            #[cfg(feature = "io-uring")]
            EventQueueKind::IoUring(_) => {
                let poller = crate::io_uring::Poller::new(fd)?;
                Ok(Poller(_Poller::IoUring(poller)))
            }
        })
    }

    #[inline]
    pub async fn wait_for_read<'a>(&'a self) -> io::Result<()> {
        match &self.0 {
            _Poller::Epoll(registration) => registration.wait_for_read().await,
            #[cfg(feature = "io-uring")]
            _Poller::IoUring(poller) => {
                let mut submitter_source = crate::io_uring::CurrentEventQueueSubmitterSource;
                poller.wait_for_read(&mut submitter_source).await
            }
        }
    }

    #[inline]
    pub async fn wait_for_write<'a>(&'a self) -> io::Result<()> {
        match &self.0 {
            _Poller::Epoll(registration) => registration.wait_for_write().await,
            #[cfg(feature = "io-uring")]
            _Poller::IoUring(poller) => {
                let mut submitter_source = crate::io_uring::CurrentEventQueueSubmitterSource;
                poller.wait_for_read(&mut submitter_source).await
            }
        }
    }
}
