use std::{collections::HashMap, convert::TryFrom, io, mem::MaybeUninit, os::unix::prelude::RawFd, sync::Arc};

use compio_internal_util::{libc_call, libc_fd_call, unix::ScopedFd};

use crate::{
    epoll::{registration::wake_with_event, Registration},
    event_queue::{EventQueueImpl, HandleImpl},
    os::linux::{EventQueueImplExt, EventQueueKind, EventQueueKindMut},
};

pub struct Epoll {
    pub(crate) shared: Arc<Shared>,
    events: HashMap<usize, RegisteredEvent>,
}

pub(crate) struct Shared {
    epoll: ScopedFd,
}

struct RegisteredEvent {
    eventfd: ScopedFd,
    callback: Box<dyn Fn(usize) + Send + Sync>,
}

impl Epoll {
    pub(crate) fn new() -> io::Result<Epoll> {
        let epoll = unsafe { libc_fd_call!(libc::epoll_create1(libc::EPOLL_CLOEXEC))? };

        Ok(Epoll {
            shared: Arc::new(Shared { epoll }),
            events: Default::default(),
        })
    }

    #[inline]
    pub(crate) fn fd(&self) -> RawFd {
        self.shared.epoll.as_raw()
    }
}

impl Shared {
    #[inline]
    pub(crate) fn fd(&self) -> RawFd {
        self.epoll.as_raw()
    }
}

impl EventQueueImpl for Epoll {
    fn poll(&self, timeout: Option<std::time::Duration>) -> std::io::Result<usize> {
        let timeout_ms = match timeout {
            Some(duration) => i32::try_from(duration.as_millis()).expect("out of range timeout"),
            None => -1,
        };

        unsafe {
            // FIXME: don't pull this number out of our asses
            let mut events: [MaybeUninit<libc::epoll_event>; 128] = MaybeUninit::uninit_array();

            let event_count = libc_call!(libc::epoll_wait(
                self.fd(),
                events.as_mut_ptr() as *mut _,
                events.len() as i32,
                timeout_ms
            ))?;

            let events = MaybeUninit::slice_assume_init_ref(&events[0..(event_count as usize)]);

            for event in events {
                // FIXME: handle registered events
                wake_with_event(event);
            }

            Ok(events.len())
        }
    }

    #[inline]
    fn poll_mut(&mut self, timeout: Option<std::time::Duration>) -> std::io::Result<usize> {
        self.poll(timeout)
    }

    #[inline]
    fn handle(&self) -> std::sync::Arc<dyn crate::event_queue::HandleImpl> {
        self.shared.clone()
    }

    fn new_custom_event(&mut self, callback: Box<dyn Fn(usize) + Send + Sync>) -> usize {
        unsafe {
            let eventfd = libc_fd_call!(libc::eventfd(
                0,
                libc::EFD_CLOEXEC | libc::EFD_SEMAPHORE | libc::EFD_NONBLOCK
            ))
            .expect("failed to create eventfd");
            let key = eventfd.as_raw() as usize;
            self.events.insert(key, RegisteredEvent { eventfd, callback });

            key
        }
    }

    fn wake(&self) -> io::Result<()> {
        todo!()
    }
}

impl HandleImpl for Shared {
    fn enqueue_custom_event(&self, key: usize, data: usize) -> io::Result<()> {
        todo!()
    }

    fn wake(&self) -> io::Result<()> {
        todo!()
    }
}

impl EventQueueImplExt for Epoll {
    #[inline]
    fn kind(&self) -> EventQueueKind {
        EventQueueKind::Epoll(self)
    }

    #[inline]
    fn kind_mut(&mut self) -> EventQueueKindMut {
        EventQueueKindMut::Epoll(self)
    }
}
