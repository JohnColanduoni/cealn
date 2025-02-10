use std::{
    cell::UnsafeCell,
    io,
    mem::{self, ManuallyDrop},
    os::unix::prelude::RawFd,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    task::{Poll, Waker},
};

use crossbeam_utils::CachePadded;
use futures::{prelude::*, ready, task::AtomicWaker};
use io_uring::{cqueue, opcode::PollAdd};
use parking_lot::lock_api::RawMutex;
use pin_project::pin_project;
use slab::Slab;
use tracing::{debug, trace, trace_span, Span};

use crate::io_uring::{submission::SubmitterSource, CompletionCallbackStorage, CompletionHandler, IoUring};

/// Manages polling operations for a file descriptor on an [`IoUring`](super::IoUring)
pub struct Poller {
    fd: RawFd,
    storage: ManuallyDrop<Pin<Box<Storage>>>,
}

impl Drop for Poller {
    fn drop(&mut self) {
        unsafe {
            let storage = self.storage.as_mut().project();
            let read_state = storage.read_callback.state.lock();
            let write_state = storage.write_callback.state.lock();
            if read_state.submitted || write_state.submitted {
                // At least one ongoing operation, we need to:
                // * Defer deleting the storage allocation until it is done
                // * Cancel the operation(s)

                // FIXME: don't leak here
            } else {
                mem::drop(read_state);
                mem::drop(write_state);
                // No ongoing operation, just release storage
                ManuallyDrop::drop(&mut self.storage);
            }
        }
    }
}

/// Poller implementation used when there is only a single operation taking place at a time
#[pin_project]
struct Storage {
    #[pin]
    // poll_state is also pointed to by the user data entry of the poll submission, so it needs to be in an `UnsafeCell`
    read_callback: CompletionCallbackStorage<Callback>,
    #[pin]
    write_callback: CompletionCallbackStorage<Callback>,
}

// Ensure header is at beginning of structure
#[repr(C)]
struct Callback {
    state: CachePadded<parking_lot::Mutex<State>>,
    // When waking, we "flip" which slab of wakers is currently active. This allows us to release the lock before
    // waking, which prevents the task we wake from being blocked by us holding the state lock.
    //
    // This is only ever accessed in the completion callback, but since we release the state lock while we are still
    // accessing this variable it is possible (though unlikely) for another completion to be scheduled on this callback
    // and for it to complete while we are still accessing the wakers.
    swapped_wakers: CachePadded<parking_lot::Mutex<Slab<Waker>>>,
}

unsafe impl Send for Callback {}
unsafe impl Sync for Callback {}

// TODO: current implementation has a thundering herd issue when there are multiple waiters. This isn't likely to be a
// big problem for most implementations, where the wakers will simple reque all tasks on the current worker thread
// (which will serialize them anyway) but we should consider if we need to do something better here.
struct State {
    submitted: bool,
    sequence_number: u64,
    wakers: Wakers,
    error: Option<i32>,
}

// Avoid allocating for the common case where we only ever have one waker at a time (i.e. only one read and one write
// operation at a time).
enum Wakers {
    None,
    Single(Waker),
    Multiple(Slab<Waker>),
}

impl Poller {
    /// Creates a structure for managing polling operations
    ///
    /// # Safety
    ///
    /// Caller must ensure that the provided file descriptor is valid and remains so until the [`Poller`] is dropped
    pub unsafe fn new(fd: RawFd) -> io::Result<Poller> {
        // Construct unique registration by default
        let storage = Box::pin(Storage {
            read_callback: CompletionCallbackStorage::new(Callback::new()),
            write_callback: CompletionCallbackStorage::new(Callback::new()),
        });
        Ok(Poller {
            fd,
            storage: ManuallyDrop::new(storage),
        })
    }

    #[inline]
    pub fn wait_for_read<'a, S: SubmitterSource>(&'a self, io_uring: &'a mut S) -> WaitForRead<'a, S> {
        WaitForRead {
            poller: self,
            wait: Wait {
                io_uring,
                submitted: None,
                span: trace_span!("WaitForRead"),
            },
        }
    }

    #[inline]
    pub fn wait_for_write<'a, S: SubmitterSource>(&'a self, io_uring: &'a mut S) -> WaitForWrite<'a, S> {
        WaitForWrite {
            poller: self,
            wait: Wait {
                io_uring,
                submitted: None,
                span: trace_span!("WaitForWrite"),
            },
        }
    }
}

impl Callback {
    fn new() -> Self {
        Callback {
            state: CachePadded::new(parking_lot::Mutex::new(State {
                submitted: false,
                sequence_number: 0,
                wakers: Wakers::None,
                error: None,
            })),
            swapped_wakers: CachePadded::new(parking_lot::Mutex::new(Slab::new())),
        }
    }
}

impl CompletionHandler for Callback {
    fn complete(&self, entry: &cqueue::Entry) {
        let mut state = self.state.lock();
        match entry.result() {
            code if code < 0 => {
                state.error = Some(-code);
            }
            _ => {}
        }
        state.submitted = false;
        match &mut state.wakers {
            Wakers::None => {}
            Wakers::Single(_) => match mem::replace(&mut state.wakers, Wakers::None) {
                Wakers::Single(waker) => {
                    // Release lock before we wake to reduce contention
                    mem::drop(state);
                    waker.wake();
                }
                _ => unreachable!(),
            },
            Wakers::Multiple(wakers) => {
                // We only access these wakers in this callback, which cannot be called concurrently because we
                // only ever submit one operation at a time
                let mut swapped_wakers = self.swapped_wakers.lock();
                mem::swap(wakers, &mut *swapped_wakers);
                // Release lock before we wake to reduce contention
                mem::drop(state);
                // Wake
                for waker in swapped_wakers.drain() {
                    waker.wake();
                }
            }
        }
    }
}

pub struct WaitForRead<'a, S: SubmitterSource> {
    poller: &'a Poller,
    wait: Wait<'a, S>,
}

impl<'a, S: SubmitterSource> Unpin for WaitForRead<'a, S> {}

impl<'a, S: SubmitterSource> Future for WaitForRead<'a, S> {
    type Output = io::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.wait.do_poll(
            this.poller.storage.as_ref().project_ref().read_callback,
            cx,
            this.poller.fd,
            libc::POLLIN as u32,
        )
    }
}

pub struct WaitForWrite<'a, S: SubmitterSource> {
    poller: &'a Poller,
    wait: Wait<'a, S>,
}

impl<'a, S: SubmitterSource> Unpin for WaitForWrite<'a, S> {}

impl<'a, S: SubmitterSource> Future for WaitForWrite<'a, S> {
    type Output = io::Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        this.wait.do_poll(
            this.poller.storage.as_ref().project_ref().read_callback,
            cx,
            this.poller.fd,
            libc::POLLOUT as u32,
        )
    }
}

struct Wait<'a, S: SubmitterSource> {
    io_uring: &'a mut S,
    submitted: Option<SubmittedWaker>,
    span: Span,
}

struct SubmittedWaker {
    sequence_number: u64,
    waker_index: usize,
}

impl<'a, S: SubmitterSource> Wait<'a, S> {
    fn do_poll(
        &mut self,
        callback: Pin<&CompletionCallbackStorage<Callback>>,
        cx: &mut std::task::Context<'_>,
        fd: RawFd,
        flags: u32,
    ) -> Poll<io::Result<()>> {
        let _guard = self.span.enter();

        let callback = callback.as_ref();
        let mut state = callback.state.lock();

        // Polling errors are currently treated as permanent
        if let Some(code) = state.error {
            return Poll::Ready(Err(io::Error::from_raw_os_error(code)));
        }

        let sequence_number = match self.submitted {
            None => {
                if !state.submitted {
                    let submit_result = self.io_uring.with_submitter(|submitter| {
                        unsafe {
                            let entry = PollAdd::new(io_uring::types::Fd(fd), flags).build();
                            ready!(submitter.push_or_wake(&entry, callback.callback(), cx.waker()));
                        }

                        state.submitted = true;
                        state.sequence_number += 1;

                        Poll::Ready(state.sequence_number)
                    });

                    ready!(submit_result)
                } else {
                    // A poll operation is already outstanding, just insert our waker
                    state.sequence_number
                }
            }
            Some(SubmittedWaker { sequence_number, .. }) => {
                if state.sequence_number != sequence_number {
                    trace!("a new poll operation was submitted, indicating caller should retry IO");
                    self.submitted = None;
                    return Poll::Ready(Ok(()));
                }
                if !state.submitted {
                    // The sequence number has not been incremented but is no longer submitted, so an event was received
                    self.submitted = None;
                    return Poll::Ready(Ok(()));
                }
                sequence_number
            }
        };

        // Still pending, add our waker
        match &mut state.wakers {
            Wakers::None => {
                state.wakers = Wakers::Single(cx.waker().clone());
                self.submitted = Some(SubmittedWaker {
                    sequence_number,
                    waker_index: 0,
                });
            }
            Wakers::Single(existing_single_waker) => match self.submitted {
                Some(SubmittedWaker { waker_index: 0, .. }) => {
                    if !existing_single_waker.will_wake(cx.waker()) {
                        // Our slot has a different waker, replace it with the current context
                        *existing_single_waker = cx.waker().clone();
                    }
                }
                Some(SubmittedWaker { .. }) => {
                    // We only ever transition from `Wakers::Single` to `Wakers::Multiple`, not the opposite. As a result,
                    // a non-zero value should be impossible here.
                    unreachable!()
                }
                None => {
                    // Transition to multiple wakers
                    match mem::replace(&mut state.wakers, Wakers::Multiple(Slab::new())) {
                        Wakers::Single(existing_single_waker) => {
                            match &mut state.wakers {
                                Wakers::Multiple(wakers) => {
                                    // For consistency, we need to insert the pre-existing waker at the first index (otherwise the
                                    // other future will get confused as to which waker is theirs).
                                    let existing_waker_index = wakers.insert(existing_single_waker);
                                    // Slab was empty, first insert should always give index 0
                                    debug_assert_eq!(existing_waker_index, 0);
                                    // Now insert our new waker
                                    let our_waker_index = wakers.insert(cx.waker().clone());
                                    self.submitted = Some(SubmittedWaker {
                                        sequence_number,
                                        waker_index: our_waker_index,
                                    });
                                }
                                _ => unreachable!(),
                            }
                        }
                        _ => unreachable!(),
                    }
                }
            },
            Wakers::Multiple(wakers) => match self.submitted {
                Some(SubmittedWaker { waker_index, .. }) => {
                    let existing_waker = &mut wakers[waker_index];
                    if !existing_waker.will_wake(cx.waker()) {
                        *existing_waker = cx.waker().clone();
                    }
                }
                None => {
                    let waker_index = wakers.insert(cx.waker().clone());
                    self.submitted = Some(SubmittedWaker {
                        sequence_number,
                        waker_index,
                    });
                }
            },
        }

        Poll::Pending
    }
}
