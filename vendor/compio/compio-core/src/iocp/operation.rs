use std::{
    io,
    marker::PhantomPinned,
    mem,
    pin::Pin,
    ptr::NonNull,
    sync::atomic::{AtomicUsize, Ordering},
    task::{Context, Poll},
};

use crossbeam_utils::CachePadded;
use futures::{prelude::*, task::AtomicWaker};
use static_assertions::assert_impl_all;
use winapi::{
    shared::{minwindef::DWORD, winerror::ERROR_IO_PENDING},
    um::{ioapiset::GetOverlappedResult, minwinbase::OVERLAPPED, winnt::STATUS_PENDING},
};
use winhandle::WinHandleRef;

/// Wraps an overlapped operation in a [`Future`](futures::future::Future)
pub struct Operation<'slot, 'handle, B> {
    slot: Option<&'slot mut OperationSlot>,
    handle: &'handle WinHandleRef,
    // NOTE: If the operation is pending or the queue has not finished processing the completion result yet this pointer
    // will not be unique.
    overlapped: NonNull<OperationOverlapped>,
    buffer: Option<B>,
    state: OperationState,
}

unsafe impl<'slot, 'handle, B> Send for Operation<'slot, 'handle, B> {}

impl<'slot, 'handle, B> Drop for Operation<'slot, 'handle, B> {
    fn drop(&mut self) {
        unsafe {
            match &self.state {
                OperationState::Panicked => {
                    // We have to assume the operation is still pending and leak everything
                    mem::forget(self.buffer.take());
                }
                OperationState::Pending => {
                    // FIXME: Try to cancel operation
                    mem::forget(self.buffer.take());
                }
                OperationState::Done | OperationState::Synchronous(_) => {
                    // Operation completed, we have unique access to the overlapped instance
                    let mut overlapped = Box::from_raw(self.overlapped.as_ptr());

                    // Give our overlapped back to the slot if possible
                    if let Some(slot) = &mut self.slot {
                        overlapped.clear();
                        slot.overlapped = Some(overlapped);
                    }
                }
            }
        }
    }
}

enum OperationState {
    Synchronous(io::Result<usize>),
    Pending,
    Done,
    Panicked,
}

/// Allows re-using allocated structures between [`Operation`](self::Operation) calls
pub struct OperationSlot {
    overlapped: Option<Box<OperationOverlapped>>,
}

unsafe impl Send for OperationSlot {}
unsafe impl Sync for OperationSlot {}

#[repr(C)]
pub(super) struct OperationOverlapped {
    // NOTE: must be the first item in the structure so we can use pointers for the two interchangably
    overlapped: OVERLAPPED,
    pub(super) waker: CachePadded<AtomicWaker>,
    _pin: PhantomPinned,
}

impl<'slot, 'handle, B> Operation<'slot, 'handle, B> {
    /// Starts an operation
    ///
    /// This function has a lot of safety assumptions, but they are mostly followed if the caller is calling a typical
    /// Win32 `OVERLAPPED` API and uses the
    /// [`RawOutputBuffer`](crate::buffer::RawOutputBuffer)/[`RawInputBuffer`](crate::buffer::RawInputBuffer) traits for buffers.
    ///
    /// The caller is responsible for the following:
    ///     * A completion result may be queued if and only if the callback returns `ERROR_IO_PENDING`. This is
    ///       the behavior of handles/sockets with `FILE_SKIP_COMPLETION_PORT_ON_SUCCESS`, but this is NOT a default.
    ///     * Any pointers that must be valid for the duration of the operation (data, `WSABUF` arrays, etc.) must
    ///       stay alive until `Drop` is called on `B`, and must stay alive indefinitely even if `B` is leaked.
    ///       Note that this is NOT the same as saying the buffers must have the lifetime of `B`: a structure holding
    ///       a reference to a `Vec` (for example) will not satisfy this property since Rust will allow dropping the
    ///       `Vec` if it can prove `B` was leaked, no matter what its destructor does (as it is not guaranteed to run).
    ///       `RawOutputBuffer`'s contract requires this for any pinned buffers obtained from its `Taken` type.
    #[inline]
    pub unsafe fn start<F>(
        mut slot: Option<&'slot mut OperationSlot>,
        handle: &'handle WinHandleRef,
        mut buffer: B,
        start: F,
    ) -> Self
    where
        F: FnOnce(&WinHandleRef, *mut OVERLAPPED, &mut B) -> io::Result<usize>,
    {
        let overlapped = slot
            .as_deref_mut()
            .and_then(|x| x.overlapped.take())
            .unwrap_or_else(|| OperationOverlapped::new());
        // NOTE: we disengage the `Box` here so if the provided callback panics, we exercise caution and leak the
        // overlapped instance. It may have been submitted.
        let overlapped = NonNull::new_unchecked(Box::into_raw(overlapped));
        match start(
            handle,
            overlapped.as_ref() as *const OperationOverlapped as *mut OVERLAPPED,
            &mut buffer,
        ) {
            Err(ref err) if err.raw_os_error() == Some(ERROR_IO_PENDING as i32) => Operation {
                slot,
                handle,
                overlapped,
                buffer: Some(buffer),
                state: OperationState::Pending,
            },
            result => {
                // The caller guarantees to use that this means the operation completed instantly and no completion
                // will be queued.
                Operation {
                    slot,
                    handle,
                    overlapped,
                    buffer: Some(buffer),
                    state: OperationState::Synchronous(result),
                }
            }
        }
    }
}

impl OperationSlot {
    #[inline]
    pub fn new() -> Self {
        OperationSlot { overlapped: None }
    }
}

impl OperationOverlapped {
    #[inline]
    pub fn new() -> Box<OperationOverlapped> {
        Box::new(OperationOverlapped {
            overlapped: unsafe { mem::zeroed() },
            waker: AtomicWaker::new().into(),
            _pin: PhantomPinned,
        })
    }

    #[inline]
    pub fn clear(self: &mut Box<Self>) {
        **self = OperationOverlapped {
            overlapped: unsafe { mem::zeroed() },
            waker: AtomicWaker::new().into(),
            _pin: PhantomPinned,
        };
    }
}

impl<'slot, 'handle, B> Future for Operation<'slot, 'handle, B> {
    type Output = (B, io::Result<usize>);

    #[inline]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        unsafe {
            let this = Pin::get_unchecked_mut(self);

            // WARNING: The state variable is used in the destructor to determine if we need to cancel/leak anything.
            //          Setting it incorrectly can result in unsafe behavior.
            let overlapped = match mem::replace(&mut this.state, OperationState::Panicked) {
                OperationState::Synchronous(result) => {
                    let buffer = this
                        .buffer
                        .take()
                        .expect("buffer should be present if Operation is not done");
                    this.state = OperationState::Done;
                    return Poll::Ready((buffer, result));
                }
                OperationState::Pending => this.overlapped.as_ref(),
                OperationState::Panicked => {
                    panic!("attempted to poll Operation after there was a panic inside `poll`");
                }
                OperationState::Done => {
                    panic!("attempted to poll Operation after a result was already emitted");
                }
            };

            // TODO: I think we need sequential consistency to synchronize with AtomicUsize, but the interaction here
            // is probably not optimal. In particular, we need only allow one "writer" to the waker field.
            if !has_overlapped_io_completed(&overlapped.overlapped, Ordering::SeqCst) {
                {
                    // Create a scope here to avoid aliasing issues
                    // TODO: we can probably do better than AtomicWaker here and avoid the double check and reduce the
                    // atomic orderings
                    overlapped.waker.register(cx.waker());
                }

                // Check if we may have missed a wakeup between checking if the IO completed and registering out waker.
                // TODO: not sure if we need sequential consistency here (AtomicWaker)
                if !has_overlapped_io_completed(&overlapped.overlapped, Ordering::SeqCst) {
                    this.state = OperationState::Pending;
                    return Poll::Pending;
                }
            }

            // Operation is complete, return the result
            let mut bytes_transferred: DWORD = 0;
            let result = if GetOverlappedResult(
                this.handle.get(),
                &overlapped.overlapped as *const OVERLAPPED as *mut OVERLAPPED,
                &mut bytes_transferred,
                0,
            ) != 0
            {
                Ok(bytes_transferred as usize)
            } else {
                Err(io::Error::last_os_error())
            };
            let buffer = this
                .buffer
                .take()
                .expect("buffer should be present if Operation is not done");
            this.state = OperationState::Done;
            Poll::Ready((buffer, result))
        }
    }
}

impl Default for OperationSlot {
    fn default() -> Self {
        OperationSlot::new()
    }
}

#[inline]
unsafe fn has_overlapped_io_completed(overlapped: *const OVERLAPPED, ordering: Ordering) -> bool {
    // We fast-path here by doing our own atomic load of the `Internal` field. In C there is a macro that does
    // this called `HasOverlappedIoCompleted` so win32 can't change this on us.
    let atomic_internal = &*(&(*overlapped).Internal as *const usize as *const AtomicUsize);
    atomic_internal.load(ordering) != STATUS_PENDING as usize
}

assert_impl_all!(OperationSlot: Send);
