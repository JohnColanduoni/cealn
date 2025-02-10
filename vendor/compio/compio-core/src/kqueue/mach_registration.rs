use std::{
    io, mem,
    os::unix::prelude::RawFd,
    pin::Pin,
    ptr,
    sync::{atomic::AtomicBool, Arc, Mutex},
    task::{Poll, Waker},
};

use crossbeam_utils::CachePadded;
use futures::prelude::*;
use mach::port::mach_port_name_t;
use slab::Slab;

use compio_internal_util::{libc_call, unix::ScopedFd};

use crate::kqueue::{event_queue, KQueue};

// FIXME: implementation is garbage, rework
#[derive(Clone)]
pub struct MachRegistration {
    shared: Arc<Shared>,
}

struct Shared {
    kqueue: Arc<event_queue::Shared>,
    port: mach_port_name_t,
    read_wait: Arc<WaitSynchronizer>,
}

struct WaitSynchronizer {
    state: CachePadded<Mutex<WaitState>>,
}

struct WaitState {
    submitted: bool,
    // Incremented each time a one-shot kqueue is re-submitted
    sequence_number: u64,
    wakers: Slab<Waker>,
}

impl MachRegistration {
    pub unsafe fn register(kqueue: &KQueue, port: mach_port_name_t) -> io::Result<MachRegistration> {
        let shared = Arc::new(Shared {
            kqueue: kqueue.shared.clone(),
            port,
            read_wait: Arc::new(WaitSynchronizer {
                state: Default::default(),
            }),
        });
        Ok(MachRegistration { shared })
    }

    pub fn wait_for_read(&self) -> WaitForRead {
        WaitForRead {
            registration: self,
            waker: None,
        }
    }
}

pub(super) fn wake_with_event(event: &libc::kevent64_s) {
    let synchronizer = unsafe { Arc::from_raw(event.udata as usize as *const WaitSynchronizer) };
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
    registration: &'a MachRegistration,
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
                let synchronizer_ptr = Arc::into_raw(self.registration.shared.read_wait.clone()) as usize as u64;
                let mut changelist = [libc::kevent64_s {
                    ident: self.registration.shared.port as u64,
                    filter: libc::EVFILT_MACHPORT,
                    flags: libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT,
                    fflags: 0,
                    data: 0,
                    // FIXME: Could leak if we unregister while submitted. Think about how to deal with this
                    udata: synchronizer_ptr as usize as u64,
                    ext: [0, 0],
                }];
                libc_call!(libc::kevent64(
                    self.registration.shared.kqueue.fd(),
                    changelist.as_mut_ptr(),
                    changelist.len() as i32,
                    ptr::null_mut(),
                    0,
                    0,
                    ptr::null_mut(),
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
