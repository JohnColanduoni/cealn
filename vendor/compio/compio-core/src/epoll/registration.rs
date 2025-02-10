use std::{
    io, mem,
    os::unix::prelude::RawFd,
    pin::Pin,
    sync::{atomic::AtomicBool, Arc, Mutex},
    task::{Poll, Waker},
};

use crossbeam_utils::CachePadded;
use futures::prelude::*;
use slab::Slab;

use compio_internal_util::{libc_call, unix::ScopedFd};

use crate::epoll::{event_queue, Epoll};

// FIXME: implementation is garbage, rework
#[derive(Clone)]
pub struct Registration {
    shared: Arc<Shared>,
}

struct Shared {
    epoll: Arc<event_queue::Shared>,

    read_fd: ScopedFd,
    read_wait: Arc<WaitSynchronizer>,

    write_fd: ScopedFd,
    write_wait: Arc<WaitSynchronizer>,
}

struct WaitSynchronizer {
    state: CachePadded<Mutex<WaitState>>,
}

struct WaitState {
    submitted: bool,
    // Incremented each time a one-shot epoll is re-submitted
    sequence_number: u64,
    wakers: Slab<Waker>,
}

impl Registration {
    pub unsafe fn register(epoll: &Epoll, fd: RawFd) -> io::Result<Registration> {
        // We duplicate the fd so we can independently control the read and write registrations
        let read_fd = duplicate_fd(fd)?;
        let write_fd = duplicate_fd(fd)?;

        let shared = Arc::new(Shared {
            epoll: epoll.shared.clone(),

            read_fd,
            read_wait: Arc::new(WaitSynchronizer {
                state: Default::default(),
            }),

            write_fd,
            write_wait: Arc::new(WaitSynchronizer {
                state: Default::default(),
            }),
        });

        let mut event = libc::epoll_event { events: 0, u64: 0 };
        libc_call!(libc::epoll_ctl(
            epoll.fd(),
            libc::EPOLL_CTL_ADD,
            shared.read_fd.as_raw(),
            &mut event
        ))?;
        libc_call!(libc::epoll_ctl(
            epoll.fd(),
            libc::EPOLL_CTL_ADD,
            shared.write_fd.as_raw(),
            &mut event
        ))?;

        Ok(Registration { shared })
    }

    pub fn wait_for_read(&self) -> WaitForRead {
        WaitForRead {
            registration: self,
            waker: None,
        }
    }

    pub fn wait_for_write(&self) -> WaitForWrite {
        WaitForWrite {
            registration: self,
            waker: None,
        }
    }
}

pub(super) fn wake_with_event(event: &libc::epoll_event) {
    let synchronizer = unsafe { Arc::from_raw(event.u64 as usize as *const WaitSynchronizer) };
    let wakers = {
        // Take wakers and set state to unsubmitted
        let mut state = synchronizer.state.lock().unwrap();
        state.submitted = false;
        mem::replace(&mut state.wakers, Slab::new())
    };
    // Wake after releasing lock to avoid contention
    for (_, waker) in wakers.into_iter() {
        waker.wake();
    }
}

struct WakerData {
    sequence_number: u64,
    waker_index: usize,
}

pub struct WaitForRead<'a> {
    registration: &'a Registration,
    waker: Option<WakerData>,
}

impl<'a> Unpin for WaitForRead<'a> {}

impl<'a> Future for WaitForRead<'a> {
    type Output = io::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let mut state = self.registration.shared.read_wait.state.lock().unwrap();
        if !state.submitted {
            if self.waker.is_some() {
                // There was a transition to an unsubmitted state while we had a submitted waker, so there has been an
                // upward edge since we started polling. Signal that the caller should retry their call
                return Poll::Ready(Ok(()));
            }

            // This is our first call and the state is unsubmitted, so we need to activate the oneshot trigger
            unsafe {
                let mut event = libc::epoll_event {
                    events: (libc::EPOLLIN | libc::EPOLLHUP | libc::EPOLLONESHOT) as u32,
                    // FIXME: Could leak if we unregister while submitted. Think about how to deal with this
                    u64: Arc::into_raw(self.registration.shared.read_wait.clone()) as usize as u64,
                };
                libc_call!(libc::epoll_ctl(
                    self.registration.shared.epoll.fd(),
                    libc::EPOLL_CTL_MOD,
                    self.registration.shared.read_fd.as_raw(),
                    &mut event
                ))?;
            }
            state.submitted = true;
        }

        // Submit our waker if needed
        let mut have_waker = false;
        if let Some(existing_waker) = self.waker.as_ref() {
            if existing_waker.sequence_number == state.sequence_number {
                have_waker = true;
                let waker_slot = &mut state.wakers[existing_waker.waker_index];
                if !waker_slot.will_wake(cx.waker()) {
                    *waker_slot = cx.waker().clone();
                }
            }
        }
        if !have_waker {
            let waker_index = state.wakers.insert(cx.waker().clone());
            self.waker = Some(WakerData {
                sequence_number: state.sequence_number,
                waker_index,
            });
        }

        Poll::Pending
    }
}

pub struct WaitForWrite<'a> {
    registration: &'a Registration,
    waker: Option<WakerData>,
}

impl<'a> Unpin for WaitForWrite<'a> {}

impl<'a> Future for WaitForWrite<'a> {
    type Output = io::Result<()>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let mut state = self.registration.shared.write_wait.state.lock().unwrap();
        if !state.submitted {
            if self.waker.is_some() {
                // There was a transition to an unsubmitted state while we had a submitted waker, so there has been an
                // upward edge since we started polling. Signal that the caller should retry their call
                return Poll::Ready(Ok(()));
            }

            // This is our first call and the state is unsubmitted, so we need to activate the oneshot trigger
            unsafe {
                let mut event = libc::epoll_event {
                    events: (libc::EPOLLOUT | libc::EPOLLHUP | libc::EPOLLONESHOT) as u32,
                    // FIXME: Could leak if we unregister while submitted. Think about how to deal with this
                    u64: Arc::into_raw(self.registration.shared.write_wait.clone()) as usize as u64,
                };
                libc_call!(libc::epoll_ctl(
                    self.registration.shared.epoll.fd(),
                    libc::EPOLL_CTL_MOD,
                    self.registration.shared.write_fd.as_raw(),
                    &mut event
                ))?;
            }
            state.submitted = true;
        }

        // Submit our waker if needed
        let mut have_waker = false;
        if let Some(existing_waker) = self.waker.as_ref() {
            if existing_waker.sequence_number == state.sequence_number {
                have_waker = true;
                let waker_slot = &mut state.wakers[existing_waker.waker_index];
                if !waker_slot.will_wake(cx.waker()) {
                    *waker_slot = cx.waker().clone();
                }
            }
        }
        if !have_waker {
            let waker_index = state.wakers.insert(cx.waker().clone());
            self.waker = Some(WakerData {
                sequence_number: state.sequence_number,
                waker_index,
            });
        }

        Poll::Pending
    }
}

impl Default for WaitState {
    fn default() -> Self {
        WaitState {
            submitted: false,
            sequence_number: 1,
            wakers: Slab::new(),
        }
    }
}

unsafe fn duplicate_fd(fd: RawFd) -> io::Result<ScopedFd> {
    let duplicate = libc::dup(fd);
    if duplicate < 0 {
        return Err(io::Error::last_os_error());
    }

    // FIXME: set cloexec

    Ok(ScopedFd::from_raw(duplicate))
}
