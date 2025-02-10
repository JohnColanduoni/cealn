use std::{
    collections::HashMap,
    convert::{TryFrom, TryInto},
    io,
    mem::MaybeUninit,
    os::unix::prelude::RawFd,
    ptr,
    sync::Arc,
};

use compio_internal_util::{libc_call, libc_fd_call, unix::ScopedFd};
use slab::Slab;

use crate::{
    event_queue::{EventQueueFactory, EventQueueImpl, HandleImpl},
    kqueue::registration::wake_with_event,
    platform::ext::EventQueueImplExt,
};

pub struct KQueue {
    pub(crate) shared: Arc<Shared>,
    events: Slab<RegisteredEvent>,
}

pub(crate) struct Shared {
    kqueue: ScopedFd,
}

struct RegisteredEvent {
    callback: Box<dyn Fn(usize) + Send + Sync>,
}

#[derive(Default)]
pub struct Options {}

impl KQueue {
    pub(crate) fn new() -> io::Result<KQueue> {
        let kqueue = unsafe { libc_fd_call!(libc::kqueue())? };

        // Add special wake event
        unsafe {
            let kevent_add = [libc::kevent64_s {
                ident: WAKE_EVENT_IDENT,
                filter: libc::EVFILT_USER,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: 0,
                data: 0,
                udata: 0,
                ext: [0; 2],
            }];
            libc_call!(libc::kevent64(
                kqueue.as_raw(),
                kevent_add.as_ptr(),
                kevent_add.len() as i32,
                ptr::null_mut(),
                0,
                0,
                ptr::null_mut()
            ))
            .unwrap();
        }

        Ok(KQueue {
            shared: Arc::new(Shared { kqueue }),
            events: Default::default(),
        })
    }

    #[inline]
    pub(crate) fn fd(&self) -> RawFd {
        self.shared.kqueue.as_raw()
    }
}

impl Shared {
    #[inline]
    pub(crate) fn fd(&self) -> RawFd {
        self.kqueue.as_raw()
    }
}

impl EventQueueImpl for KQueue {
    fn poll(&self, timeout: Option<std::time::Duration>) -> std::io::Result<usize> {
        let timeout_ms = timeout.map(|duration| libc::timespec {
            tv_sec: duration.as_secs() as i64,
            tv_nsec: duration.subsec_nanos() as i64,
        });

        unsafe {
            // FIXME: don't pull this number out of our asses
            let mut events: [MaybeUninit<libc::kevent64_s>; 128] = MaybeUninit::uninit_array();

            let event_count = libc_call!(libc::kevent64(
                self.fd(),
                ptr::null_mut(),
                0,
                events.as_mut_ptr() as *mut _,
                events.len() as i32,
                0,
                timeout_ms
                    .as_ref()
                    .map(|x| x as *const libc::timespec)
                    .unwrap_or(ptr::null())
            ))?;

            let events = MaybeUninit::slice_assume_init_ref(&events[0..(event_count as usize)]);

            for event in events {
                // FIXME: handle registered events
                if event.filter == libc::EVFILT_READ || event.filter == libc::EVFILT_WRITE {
                    wake_with_event(event);
                    continue;
                }
                #[cfg(target_os = "macos")]
                if event.filter == libc::EVFILT_MACHPORT {
                    crate::kqueue::mach_registration::wake_with_event(event);
                    continue;
                }
                if event.filter == libc::EVFILT_USER {
                    if event.ident == WAKE_EVENT_IDENT {
                        continue;
                    }
                    let index: usize = event.ident.try_into().unwrap();
                    let callback = self.events.get(index).expect("invalid EVFILT_USER event received");
                    (callback.callback)(event.udata as usize);
                    continue;
                }
                panic!("unknown event inserted into queue");
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
            let index = self.events.insert(RegisteredEvent { callback });
            let kevent_add = [libc::kevent64_s {
                ident: index as u64,
                filter: libc::EVFILT_USER,
                flags: libc::EV_ADD | libc::EV_CLEAR,
                fflags: 0,
                data: 0,
                udata: 0,
                ext: [0; 2],
            }];
            libc_call!(libc::kevent64(
                self.fd(),
                kevent_add.as_ptr(),
                kevent_add.len() as i32,
                ptr::null_mut(),
                0,
                0,
                ptr::null_mut()
            ))
            .unwrap();
            index
        }
    }

    fn wake(&self) -> io::Result<()> {
        unsafe {
            let kevent_trigger = [libc::kevent64_s {
                ident: WAKE_EVENT_IDENT,
                filter: libc::EVFILT_USER,
                flags: 0,
                fflags: libc::NOTE_TRIGGER,
                data: 0,
                udata: 0,
                ext: [0; 2],
            }];
            libc_call!(libc::kevent64(
                self.fd(),
                kevent_trigger.as_ptr(),
                kevent_trigger.len() as i32,
                ptr::null_mut(),
                0,
                0,
                ptr::null_mut()
            ))?;
            Ok(())
        }
    }
}

const WAKE_EVENT_IDENT: u64 = u64::MAX;

impl HandleImpl for Shared {
    fn enqueue_custom_event(&self, key: usize, data: usize) -> io::Result<()> {
        unsafe {
            let kevent_trigger = [libc::kevent64_s {
                ident: key as u64,
                filter: libc::EVFILT_USER,
                flags: 0,
                fflags: libc::NOTE_TRIGGER,
                data: 0,
                udata: data as u64,
                ext: [0; 2],
            }];
            libc_call!(libc::kevent64(
                self.fd(),
                kevent_trigger.as_ptr(),
                kevent_trigger.len() as i32,
                ptr::null_mut(),
                0,
                0,
                ptr::null_mut()
            ))?;
            Ok(())
        }
    }

    fn wake(&self) -> io::Result<()> {
        todo!()
    }
}

impl EventQueueImplExt for KQueue {
    #[inline]
    fn kqueue(&self) -> &KQueue {
        self
    }
}

impl EventQueueFactory for Options {
    fn new(&self) -> io::Result<crate::EventQueue> {
        let kqueue = KQueue::new()?;
        Ok(crate::EventQueue::with_imp(Box::new(kqueue)))
    }
}
