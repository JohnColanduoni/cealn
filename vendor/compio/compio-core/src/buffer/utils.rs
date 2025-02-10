use std::{
    mem::{self, ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    slice,
};

use tracing::warn;

use super::{InputBufferVisitor, OutputBufferVisitor, RawInputBuffer, RawOutputBuffer};

/// A convenience wrapper that converts a [`RawOutputBuffer`](self::RawOutputBuffer) to a guaranteed pinned contiguous buffer
///
/// This guard also ensures that the buffer is taken and held until explicitly released with [`release`](PinnedOutputGuard::release)
/// (i.e. it prevents releasing the buffer when the calling future is dropped without succesful cancelation).
pub fn ensure_pinned_output<B>(buffer: B) -> PinnedOutputGuard<B>
where
    B: RawOutputBuffer,
{
    let mut taken = buffer.take();
    let state = B::visit(&mut taken, EnsurePinnedOutputVisitor);
    PinnedOutputGuard {
        taken: ManuallyDrop::new(taken),
        state,
    }
}

pub struct PinnedOutputGuard<B>
where
    B: RawOutputBuffer,
{
    taken: ManuallyDrop<B::Taken>,
    state: PinnedOutputGuardState,
}

impl<B: RawOutputBuffer> Drop for PinnedOutputGuard<B> {
    fn drop(&mut self) {
        match &self.state {
            PinnedOutputGuardState::Released => {}
            _ => {
                warn!("leaking pinned output buffer because cannot guarantee it is not still in use");
            }
        }
    }
}

unsafe impl<B: RawOutputBuffer> Send for PinnedOutputGuard<B> {}
unsafe impl<B: RawOutputBuffer> Sync for PinnedOutputGuard<B> {}

enum PinnedOutputGuardState {
    Copy(ManuallyDrop<Vec<u8>>),
    Pinned { ptr: *const u8, len: usize },
    Released,
}

struct EnsurePinnedOutputVisitor;

impl<'a> OutputBufferVisitor for EnsurePinnedOutputVisitor {
    type Output = PinnedOutputGuardState;

    #[inline]
    fn unpinned_slice(self, data: &[u8]) -> Self::Output {
        #[cfg(feature = "perf-warnings")]
        tracing::warn!("a copy was required to provide a pinned buffer for an IO operation");

        PinnedOutputGuardState::Copy(ManuallyDrop::new(data.to_vec()))
    }

    #[inline]
    fn unpinned_vector(self, data: &[&[u8]]) -> Self::Output {
        #[cfg(feature = "perf-warnings")]
        tracing::warn!("a copy was required to provide a (pinned) continguous buffer for an IO operation");

        let mut copy = Vec::with_capacity(data.iter().map(|x| x.len()).sum());
        for segment in data {
            copy.extend_from_slice(segment);
        }
        PinnedOutputGuardState::Copy(ManuallyDrop::new(copy))
    }

    #[inline]
    unsafe fn pinned_slice(self, data: &[u8]) -> Self::Output {
        PinnedOutputGuardState::Pinned {
            ptr: data.as_ptr(),
            len: data.len(),
        }
    }
}

impl<B: RawOutputBuffer> PinnedOutputGuard<B> {
    /// Releases the buffer
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the buffer is no longer in use by the system.
    #[inline]
    pub unsafe fn release(mut self) {
        match mem::replace(&mut self.state, PinnedOutputGuardState::Released) {
            PinnedOutputGuardState::Copy(buffer) => {
                // Release output buffer
                mem::drop(ManuallyDrop::into_inner(buffer));
            }
            PinnedOutputGuardState::Pinned { .. } => {
                // Data was written directly into provided buffer, no need to do anything
            }
            PinnedOutputGuardState::Released => unreachable!(),
        }
        // Release original buffer taken handle
        ManuallyDrop::drop(&mut self.taken);
    }
}

impl<B> Deref for PinnedOutputGuard<B>
where
    B: RawOutputBuffer,
{
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        match &self.state {
            PinnedOutputGuardState::Copy(buffer) => buffer,
            PinnedOutputGuardState::Pinned { ptr, len } => unsafe { slice::from_raw_parts(*ptr, *len) },
            PinnedOutputGuardState::Released => unreachable!(),
        }
    }
}

/// A convenience wrapper that converts a [`RawOutputBuffer`](self::RawOutputBuffer) to a guaranteed pinned contiguous buffer
pub fn ensure_pinned_input<B>(buffer: B) -> PinnedInputGuard<B>
where
    B: RawInputBuffer,
{
    let mut taken = buffer.take();
    let state = B::visit(&mut taken, EnsurePinnedInputVisitor);
    PinnedInputGuard {
        taken: ManuallyDrop::new(taken),
        state,
    }
}

pub struct PinnedInputGuard<B>
where
    B: RawInputBuffer,
{
    taken: ManuallyDrop<B::Taken>,
    state: PinnedInputGuardState,
}

impl<B: RawInputBuffer> Drop for PinnedInputGuard<B> {
    fn drop(&mut self) {
        match &self.state {
            PinnedInputGuardState::Released => {}
            _ => {
                warn!("leaking pinned input buffer because cannot guarantee it is not still in use");
            }
        }
    }
}

unsafe impl<B: RawInputBuffer> Send for PinnedInputGuard<B> {}

enum PinnedInputGuardState {
    Allocated(ManuallyDrop<Vec<u8>>),
    Pinned { ptr: *mut MaybeUninit<u8>, len: usize },
    Released,
}

struct EnsurePinnedInputVisitor;

struct CopyPinnedInputVisitor<'a> {
    data: &'a [u8],
}

impl<B: RawInputBuffer> PinnedInputGuard<B> {
    /// Finalizes the buffer with the read bytes, performing a copy if needed
    ///
    /// # Safety
    ///
    /// The caller must guarantee that:
    /// * The `bytes_read` value accurately reflects the portion of the buffer that was initialized with data and
    ///   that it is in range.
    /// * The buffer is no longer in use by the system
    #[inline]
    pub unsafe fn finalize(mut self, bytes_read: usize) {
        let taken = match mem::replace(&mut self.state, PinnedInputGuardState::Released) {
            PinnedInputGuardState::Allocated(mut buffer) => {
                // Copy into actual input buffer
                let mut taken = ManuallyDrop::take(&mut self.taken);
                buffer.set_len(bytes_read);
                B::visit(&mut taken, CopyPinnedInputVisitor { data: &buffer });
                ManuallyDrop::drop(&mut buffer);
                taken
            }
            PinnedInputGuardState::Pinned { .. } => {
                // Data was written directly into provided buffer, no need to do anything
                ManuallyDrop::take(&mut self.taken)
            }
            PinnedInputGuardState::Released => unreachable!(),
        };
        B::finalize(taken, bytes_read)
    }

    /// Releases the buffer
    ///
    /// Note that this function should only be used if the operation failed and the buffer was never initialized
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the buffer is no longer in use by the system.
    #[inline]
    pub unsafe fn release(mut self) {
        match mem::replace(&mut self.state, PinnedInputGuardState::Released) {
            PinnedInputGuardState::Allocated(buffer) => {
                // Release input buffer
                mem::drop(ManuallyDrop::into_inner(buffer));
            }
            PinnedInputGuardState::Pinned { .. } => {
                // Data was written directly into provided buffer, no need to do anything
            }
            PinnedInputGuardState::Released => unreachable!(),
        }
        // Release taken original buffer
        let mut taken = ManuallyDrop::take(&mut self.taken);
        mem::drop(taken);
    }
}

impl InputBufferVisitor for EnsurePinnedInputVisitor {
    type Output = PinnedInputGuardState;

    #[inline]
    fn unpinned_slice(self, buffer: &mut [MaybeUninit<u8>]) -> Self::Output {
        #[cfg(feature = "perf-warnings")]
        tracing::warn!("an allocation was required to provide a pinned input buffer for an IO operation");

        PinnedInputGuardState::Allocated(ManuallyDrop::new(Vec::with_capacity(buffer.len())))
    }

    #[inline]
    fn unpinned_vector(self, buffers: &[&mut [MaybeUninit<u8>]]) -> Self::Output {
        #[cfg(feature = "perf-warnings")]
        tracing::warn!("a copy was required to provide a (pinned) continguous input buffer for an IO operation");

        let total_len = buffers.iter().map(|x| x.len()).sum();
        PinnedInputGuardState::Allocated(ManuallyDrop::new(Vec::with_capacity(total_len)))
    }

    #[inline]
    unsafe fn pinned_slice(self, buffer: &mut [MaybeUninit<u8>]) -> Self::Output {
        PinnedInputGuardState::Pinned {
            ptr: buffer.as_mut_ptr(),
            len: buffer.len(),
        }
    }
}

impl<'a> InputBufferVisitor for CopyPinnedInputVisitor<'a> {
    type Output = ();

    #[inline]
    fn unpinned_slice(self, buffer: &mut [MaybeUninit<u8>]) -> Self::Output {
        unsafe {
            assert!(buffer.len() >= self.data.len());
            std::ptr::copy(self.data.as_ptr(), buffer.as_mut_ptr() as *mut u8, self.data.len());
        }
    }

    #[inline]
    fn unpinned_vector(self, buffers: &[&mut [MaybeUninit<u8>]]) -> Self::Output {
        todo!()
    }

    #[inline]
    unsafe fn pinned_slice(self, _buffer: &mut [MaybeUninit<u8>]) -> Self::Output {
        unreachable!()
    }
}

impl<B> Deref for PinnedInputGuard<B>
where
    B: RawInputBuffer,
{
    type Target = [MaybeUninit<u8>];

    #[inline]
    fn deref(&self) -> &[MaybeUninit<u8>] {
        match &self.state {
            PinnedInputGuardState::Allocated(buffer) => unsafe {
                slice::from_raw_parts(buffer.as_ptr() as *const MaybeUninit<u8>, buffer.capacity())
            },
            PinnedInputGuardState::Pinned { ptr, len } => unsafe { slice::from_raw_parts(*ptr, *len) },
            PinnedInputGuardState::Released => unreachable!(),
        }
    }
}

impl<B> DerefMut for PinnedInputGuard<B>
where
    B: RawInputBuffer,
{
    #[inline]
    fn deref_mut(&mut self) -> &mut [MaybeUninit<u8>] {
        match &mut self.state {
            PinnedInputGuardState::Allocated(buffer) => unsafe {
                slice::from_raw_parts_mut(buffer.as_mut_ptr() as *mut MaybeUninit<u8>, buffer.capacity())
            },
            PinnedInputGuardState::Pinned { ptr, len } => unsafe { slice::from_raw_parts_mut(*ptr, *len) },
            PinnedInputGuardState::Released => unreachable!(),
        }
    }
}
