use std::{
    cell::UnsafeCell,
    ffi::c_int,
    io,
    marker::PhantomPinned,
    mem,
    ops::{Deref, Sub},
    pin::Pin,
    ptr::NonNull,
    task::{Poll, Waker},
};

use compio_internal_util::{libc_call, unix::ScopedFd};
use crossbeam_utils::CachePadded;
use futures::prelude::*;
use io_uring::{cqueue, squeue};
use pin_project::pin_project;

use crate::io_uring::SubmitterSource;

pub trait CompletionHandler: Send + Sync + Sized {
    fn complete(&self, entry: &cqueue::Entry);
}

/// Stores a completion callback suitable
#[repr(C)]
pub struct CompletionCallbackStorage<T> {
    header: CompleterHeader,
    // Unique ownership of the storage does not imply unique access to the contained data, as the callback may be
    // called concurrently
    data: UnsafeCell<T>,
    _pin: PhantomPinned,
}

unsafe impl<T: CompletionHandler> Send for CompletionCallbackStorage<T> {}
unsafe impl<T: CompletionHandler> Sync for CompletionCallbackStorage<T> {}

pub struct CompletionCallback(NonNull<CompleterHeader>);

struct CompleterHeader {
    callback: unsafe fn(ptr: *const CompleterHeader, entry: &cqueue::Entry),
}

#[repr(C)]
pub(super) struct CustomEventCompleter {
    header: CompleterHeader,
    callback: Box<dyn Fn(usize) + Send + Sync>,
    pipe: ScopedFd,
}

#[repr(C)]
pub(super) struct WakeCompleter {
    header: CompleterHeader,
    event_fd: c_int,
}

impl<T: CompletionHandler> CompletionCallbackStorage<T> {
    #[inline]
    pub fn new(data: T) -> CompletionCallbackStorage<T> {
        CompletionCallbackStorage {
            header: CompleterHeader {
                callback: completer_callback_adapter::<T>,
            },
            data: UnsafeCell::new(data),
            _pin: PhantomPinned,
        }
    }

    #[inline]
    pub fn project(self: Pin<&Self>) -> Pin<&T> {
        unsafe { Pin::new_unchecked(self.get_ref().deref()) }
    }

    /// Constructs a reference to this completer suitable for submission
    ///
    /// # Safety
    ///
    /// Caller is responsible for ensuring the pinned [`CompletionCallbackStorage`] lives longer than the
    /// [`CompletionCallback`] submission.
    #[inline]
    pub unsafe fn callback(self: Pin<&Self>) -> CompletionCallback {
        CompletionCallback(NonNull::new_unchecked(
            &self.header as *const CompleterHeader as *mut CompleterHeader,
        ))
    }
}

impl CompletionCallback {
    pub(crate) unsafe fn from_user_data(user_data: u64) -> Self {
        CompletionCallback(NonNull::new(user_data as usize as *mut CompleterHeader).unwrap())
    }

    #[inline]
    pub(crate) unsafe fn call(self, entry: &cqueue::Entry) {
        unsafe {
            let header = self.0.as_ref();
            (header.callback)(header, entry);
        }
    }

    pub(crate) fn user_data(&self) -> u64 {
        self.0.as_ptr() as usize as u64
    }
}

impl CustomEventCompleter {
    pub(super) fn new(callback: Box<dyn Fn(usize) + Send + Sync>, pipe: ScopedFd) -> Self {
        CustomEventCompleter {
            header: CompleterHeader {
                callback: custom_event_callback_adapter,
            },
            callback,
            pipe,
        }
    }
}

unsafe fn custom_event_callback_adapter(ptr: *const CompleterHeader, entry: &cqueue::Entry) {
    let storage = &*(ptr as *const CustomEventCompleter);
    let mut userdata_buffer = [0u8; mem::size_of::<u64>()];
    match libc_call!(libc::read(
        storage.pipe.as_raw(),
        userdata_buffer.as_mut_ptr() as _,
        userdata_buffer.len()
    )) {
        Ok(n) if n == userdata_buffer.len() as isize => {
            (storage.callback)(u64::from_ne_bytes(userdata_buffer) as usize)
        }
        Ok(_) => todo!(),
        Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
            // No message, this is fine
        }
        Err(err) => panic!("read from custom event pipe failed: {:?}", err),
    }
}

impl WakeCompleter {
    pub(super) fn new(event_fd: c_int) -> WakeCompleter {
        WakeCompleter {
            header: CompleterHeader {
                callback: wake_callback,
            },
            event_fd,
        }
    }
}

unsafe fn wake_callback(ptr: *const CompleterHeader, entry: &cqueue::Entry) {
    let storage = &*(ptr as *const WakeCompleter);
    let mut userdata_buffer = [0u8; mem::size_of::<u64>()];
    match libc_call!(libc::read(
        storage.event_fd,
        userdata_buffer.as_mut_ptr() as _,
        userdata_buffer.len()
    )) {
        Ok(_) => {}
        Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
            // No message, this is fine
        }
        Err(err) => panic!("read from wake eventfd failed: {:?}", err),
    }
}

impl<T> Deref for CompletionCallbackStorage<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe { &*(self.data.get() as *const T) }
    }
}

unsafe fn completer_callback_adapter<T: CompletionHandler>(ptr: *const CompleterHeader, entry: &cqueue::Entry) {
    let storage = &*(ptr as *const CompletionCallbackStorage<T>);
    T::complete(&*(storage.data.get() as *const T), entry);
}

/// A [`CompletionHandler`] container which implements a simple [`Future`](futures::Future) waking protocol
#[pin_project]
pub struct CompletionWakerStorage {
    #[pin]
    storage: CompletionCallbackStorage<CompletionWakerData>,
}

struct CompletionWakerData {
    state: CachePadded<parking_lot::Mutex<CompletionWakerState>>,
}

struct CompletionWakerState {
    result: Option<i32>,
    waker: Option<Waker>,
    submitted: bool,
    // In the event the holder of this storage wants to release it while an operation is outstanding, it will instead
    // give ownership of the cleanup of the allocation holding this storage to this callback. This allows the allocation
    // to be released only after the completion finishes.
    cleanup: Option<Box<dyn FnOnce() + Send>>,
}

pub struct CompletionWakerSubmission<'a, S: SubmitterSource, F: FnOnce() -> squeue::Entry + Unpin> {
    storage: Pin<&'a mut CompletionWakerStorage>,
    submitter: &'a mut S,
    submission: Option<squeue::Entry>,
    prepare: Option<F>,
}

impl CompletionWakerStorage {
    pub fn new() -> CompletionWakerStorage {
        CompletionWakerStorage {
            storage: CompletionCallbackStorage::new(CompletionWakerData {
                state: CachePadded::new(parking_lot::Mutex::new(CompletionWakerState {
                    result: None,
                    waker: None,
                    submitted: false,
                    cleanup: None,
                })),
            }),
        }
    }

    /// Submits an operation to an [`IoUring`](crate::io_uring::IoUring) using this storage for synchronization
    ///
    /// The preparation function is guaranteed to only be called after any previous operation using this storage has
    /// completed or finished being canceled. It is useful if there is any other pinned data that may be pointed to by
    /// previous operations (e.g. buffers).
    ///
    /// # Safety
    ///
    /// The caller is responsible for ensuring that the allocation containg the storage is not released before the
    /// operation completes or is canceled. This is most easily done using the [`ensure_cleanup`](Self::ensure_cleanup)
    /// function.
    #[inline]
    pub unsafe fn submit<'a, S: SubmitterSource, F: FnOnce() -> squeue::Entry + Unpin>(
        self: Pin<&'a mut Self>,
        submitter: &'a mut S,
        prepare: F,
    ) -> CompletionWakerSubmission<S, F> {
        CompletionWakerSubmission {
            storage: self,
            submitter,
            submission: None,
            prepare: Some(prepare),
        }
    }

    /// Ensures a cleanup function runs only after the current submission is complete
    ///
    /// If there is an active submission pointing to this storage, it is not safe to release the allocation holding it
    /// as the completion callback will access it. To ease the handling of this case, callers should pass the
    /// responsibility for cleaning up the containing allocation to this function.
    ///
    /// If there is no active submission, the cleanup callback will be called immediately. Otherwise it will be stored
    /// and called by the completion callback once the operation is complete and this storage is no longer needed.
    ///
    /// # Safety
    ///
    /// This function takes a pointer to the instance because the allocation holding the instance may be deleted by
    /// the time this function returns. The caller is responsible for ensuring that the pointer is valid at the time
    /// of the call, and that it stays valid until the provided cleanup callback is called.
    pub unsafe fn ensure_cleanup<F>(this: *mut Self, cleanup: F)
    where
        F: FnOnce() + Send + 'static,
    {
        let this = &*(this as *const Self);
        let mut state = this.storage.state.lock();
        if !state.submitted {
            // Fast path, memory is not in use and can be cleaned up immediately

            // We must release the lock before calling cleanup, because the pointer may no longer be valid after it runs
            mem::drop(state);
            mem::drop(this);
            cleanup();
        } else {
            debug_assert!(state.cleanup.is_none());
            state.cleanup = Some(Box::new(cleanup));

            // NOTE: as soon the lock is released, it is possible for the cleanup callback to be called on another
            // thread. The provided pointer MUST NOT be accessed after the unlock.
            mem::drop(state);
            mem::drop(this);
        }
    }
}

impl CompletionHandler for CompletionWakerData {
    #[inline]
    fn complete(&self, entry: &cqueue::Entry) {
        let mut state = self.state.lock();
        debug_assert!(!state.result.is_some());
        state.result = Some(entry.result());
        state.submitted = false;
        let waker = state.waker.take();
        let cleanup = state.cleanup.take();
        // Release lock before waking to reduce chance of contention
        mem::drop(state);
        if let Some(waker) = waker {
            waker.wake();
        }
        // NOTE: the cleanup callback cannot be called until the lock is released, as the cleanup may cause the self
        // pointer to become invalid
        if let Some(cleanup) = cleanup {
            cleanup();
        }
    }
}

impl<'a, S: SubmitterSource, F: FnOnce() -> squeue::Entry + Unpin> Future for CompletionWakerSubmission<'a, S, F> {
    type Output = io::Result<i32>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        let this = &mut *self;
        let data = this.storage.as_mut().project().storage;
        let mut state = data.state.lock();

        if let Some(prepare) = this.prepare.take() {
            if state.submitted {
                todo!("cancel existing submission");
            }
            this.submission = Some(prepare());
        }

        if let Some(entry) = this.submission.take() {
            // We should have handled the already submitted case when calling the prepare function
            debug_assert!(!state.submitted);
            let submit_result = this.submitter.with_submitter(|submitter| unsafe {
                submitter.push_or_wake(&entry, data.as_ref().callback(), cx.waker())
            });
            match submit_result {
                Poll::Ready(()) => {
                    state.submitted = true;
                }
                Poll::Pending => {
                    // Put the entry back since we weren't able to submit
                    this.submission = Some(entry);
                    return Poll::Pending;
                }
            }
        }

        if let Some(result) = state.result.take() {
            let result = match result {
                code if code < 0 => Err(io::Error::from_raw_os_error(-code)),
                code => Ok(code),
            };
            return Poll::Ready(result);
        }

        let will_wake = state
            .waker
            .as_ref()
            .map(|waker| cx.waker().will_wake(waker))
            .unwrap_or(false);
        if !will_wake {
            state.waker = Some(cx.waker().clone());
        }

        Poll::Pending
    }
}
