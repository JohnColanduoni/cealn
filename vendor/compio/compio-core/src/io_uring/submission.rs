use std::{
    sync::Arc,
    task::{Poll, Waker},
};

use io_uring::{squeue::Entry, SubmissionQueue};
use slab::Slab;
use tracing::debug;

use crate::{
    io_uring::{CompletionCallback, IoUring},
    os::linux::{EventQueueExt, EventQueueKindMut},
    EventQueue,
};

pub struct Submitter<'a> {
    submitter: _Submitter<'a>,
    submission_full_wake_state: Arc<SubmissionFullWakeState>,
}

pub(super) struct SubmissionFullWakeState {
    wakers: parking_lot::Mutex<Slab<Waker>>,
}

pub trait SubmitterSource {
    fn with_submitter<F, T>(&mut self, f: F) -> T
    where
        for<'a> F: FnOnce(&mut Submitter<'a>) -> T;
}

enum _Submitter<'a> {
    Direct(SubmissionQueue<'a>),
}

impl<'a> Submitter<'a> {
    /// # Safety
    ///
    /// The caller must ensure the following:
    /// * The provided entry is safe to execute according to io_uring semantics, including but not limited to keeping
    ///   any pointed to entities alive until the corresponding completion event comes back.
    /// * The callback must be kept alive until this entry completes
    pub unsafe fn push(&mut self, entry: Entry, callback: CompletionCallback) -> Poll<()> {
        todo!()
    }

    /// # Safety
    ///
    /// The caller must ensure the following:
    /// * The provided entry is safe to execute according to io_uring semantics, including but not limited to keeping
    ///   any pointed to entities alive until the corresponding completion event comes back.
    /// * The user data must be a [`CompletionRef`](super::CompletionRef)
    pub unsafe fn push_or_wake(
        &mut self,
        entry: &Entry,
        callback: CompletionCallback,
        wake_on_push_read: &Waker,
    ) -> Poll<()> {
        match &mut self.submitter {
            _Submitter::Direct(submission_queue) => {
                match submission_queue.push(&entry.clone().user_data(callback.user_data())) {
                    Ok(()) => Poll::Ready(()),
                    Err(_) => {
                        let mut wake_state = self.submission_full_wake_state.wakers.lock();
                        wake_state.insert(wake_on_push_read.clone());
                        debug!("waiting for space to submit work");
                        Poll::Pending
                    }
                }
            }
        }
    }

    /// # Safety
    ///
    /// The caller must ensure the following:
    /// * The provided entry is safe to execute according to io_uring semantics, including but not limited to keeping
    ///   any pointed to entities alive until the corresponding completion event comes back.
    /// * The callback must be kept alive until this entry completes
    pub unsafe fn push_sequential_or_wake<'b, I>(
        &mut self,
        entries: I,
        callback: CompletionCallback,
        wake_on_push_ready: &Waker,
    ) -> Poll<()>
    where
        I: IntoIterator<Item = &'b Entry>,
        I::IntoIter: ExactSizeIterator,
    {
        todo!()
    }
}

pub struct CurrentEventQueueSubmitterSource;

impl SubmitterSource for CurrentEventQueueSubmitterSource {
    fn with_submitter<F, T>(&mut self, f: F) -> T
    where
        for<'a> F: FnOnce(&mut Submitter<'a>) -> T,
    {
        EventQueue::with_current_mut_or_else(
            |event_queue| match event_queue.kind_mut() {
                EventQueueKindMut::IoUring(io_uring) => SubmitterSource::with_submitter(io_uring, f),
                _ => panic!("current event queue is not an io_uring, and this file descriptor was set up for io_uring"),
            },
            |event_queue| todo!(),
        )
    }
}

impl<'a> SubmitterSource for IoUring {
    #[inline]
    fn with_submitter<F, T>(&mut self, f: F) -> T
    where
        for<'b> F: FnOnce(&mut Submitter<'b>) -> T,
    {
        let mut submitter = Submitter {
            submitter: _Submitter::Direct(self.ring.submission()),
            submission_full_wake_state: self.shared.submission_full_wake_state.clone(),
        };
        f(&mut submitter)
    }
}

impl SubmissionFullWakeState {
    pub(super) fn new() -> SubmissionFullWakeState {
        SubmissionFullWakeState {
            wakers: Default::default(),
        }
    }

    pub(super) fn wake(&self) {
        let mut lock = self.wakers.lock();
        // It's fine to keep this lock while we wake since the tasks we wake only take the lock if there isn't space
        // in the queue, which is unlikely
        let mut wake_count = 0;
        for waker in lock.drain() {
            wake_count += 1;
            waker.wake()
        }
        if wake_count > 0 {
            debug!("woke tasks waiting for submission space");
        }
    }
}
