use std::{
    mem,
    ops::{Deref, DerefMut},
    slice,
};

use bytes::{buf::UninitSlice, BufMut};
use mem::{ManuallyDrop, MaybeUninit};

use crate::{
    buffer::{
        alloc::AllocBuffer, BufferRequest, InputBuffer, OutputBuffer, OutputBufferVisitor, RawInputBuffer,
        RawOutputBuffer,
    },
    iocp::OperationSlot,
};

/// A [`RawInputBuffer`](crate::buffer::RawInputBuffer)/[`RawOutputBuffer`](crate::buffer::RawOutputBuffer) optimized for
/// general-purpose overlapped IO.
///
/// This consists of both a byte buffer allocated on the heap by the default allocator and an
/// [`OperationSlot`](crate::iocp::OperationSlot) for reusing the underlying `OVERLAPPED` structure. Optimal for most
/// Win32 APIs.
pub struct OperationAllocBuffer {
    // FIXME: use this for operations
    _slot: OperationSlot,
    buffer: AllocBuffer,
}

impl OperationAllocBuffer {
    #[inline]
    pub fn new(capacity: usize) -> Self {
        OperationAllocBuffer {
            _slot: OperationSlot::new(),
            buffer: AllocBuffer::new(BufferRequest::new(capacity)),
        }
    }

    #[inline]
    pub fn with_traits(request: BufferRequest) -> Self {
        OperationAllocBuffer {
            _slot: OperationSlot::new(),
            buffer: AllocBuffer::new(request),
        }
    }
}

impl OutputBuffer for OperationAllocBuffer {
    #[inline]
    fn clear(&mut self) {
        self.buffer.clear()
    }

    #[inline]
    fn as_contiguous(&self) -> Option<&[u8]> {
        Some(&self.buffer)
    }

    #[inline]
    fn as_contiguous_mut(&mut self) -> Option<&mut [u8]> {
        Some(&mut self.buffer)
    }
}

impl InputBuffer for OperationAllocBuffer {
    #[inline]
    fn len(&self) -> usize {
        self.buffer.len()
    }

    #[inline]
    fn chunk_at(&self, offset: usize) -> &[u8] {
        &self.buffer[offset..]
    }

    #[inline]
    fn as_contiguous(&self) -> Option<&[u8]> {
        Some(&self.buffer)
    }
}

pub struct TakenOperationAllocOutputBuffer<'a> {
    dest: &'a mut OperationAllocBuffer,
    buffer: ManuallyDrop<AllocBuffer>,
}

impl<'a> Drop for TakenOperationAllocOutputBuffer<'a> {
    fn drop(&mut self) {
        unsafe {
            // This is safe because we only ever move `buffer` here (and don't access it afterwards)
            self.dest.buffer = ManuallyDrop::take(&mut self.buffer);
        }
    }
}

impl<'a> RawOutputBuffer for &'a mut OperationAllocBuffer {
    type Taken = TakenOperationAllocOutputBuffer<'a>;

    #[inline]
    fn take(self) -> Self::Taken {
        let buffer = mem::replace(&mut self.buffer, AllocBuffer::empty());
        TakenOperationAllocOutputBuffer {
            dest: self,
            buffer: ManuallyDrop::new(buffer),
        }
    }

    #[inline]
    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: OutputBufferVisitor,
    {
        unsafe { visitor.pinned_slice(&taken.buffer) }
    }
}

pub struct TakenOperationAllocInputBuffer<'a> {
    dest: &'a mut OperationAllocBuffer,
    buffer: Option<AllocBuffer>,
}

impl<'a> Drop for TakenOperationAllocInputBuffer<'a> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            // Buffer was never finalized, just put it back
            self.dest.buffer = buffer;
        }
    }
}

impl<'a> RawInputBuffer for &'a mut OperationAllocBuffer {
    type Taken = TakenOperationAllocInputBuffer<'a>;

    #[inline]
    fn take(self) -> Self::Taken {
        let buffer = mem::replace(&mut self.buffer, AllocBuffer::empty());
        TakenOperationAllocInputBuffer {
            dest: self,
            buffer: Some(buffer),
        }
    }

    #[inline]
    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: crate::buffer::InputBufferVisitor,
    {
        let buffer = taken.buffer.as_mut().expect("already finalized");
        let capacity = buffer.capacity();
        let len = buffer.len();
        unsafe {
            visitor.pinned_slice(
                &mut slice::from_raw_parts_mut(buffer.as_mut_ptr() as *mut MaybeUninit<u8>, capacity)[len..],
            )
        }
    }

    #[inline]
    fn finalize(mut taken: Self::Taken, bytes_read: usize) {
        let mut buffer = taken.buffer.take().expect("already finalized");
        let capacity = buffer.capacity();
        let len = buffer.len();

        unsafe {
            assert!(
                len + bytes_read <= capacity,
                "more bytes read than were made available by buffer"
            );

            buffer.set_len(len + bytes_read);
        }

        taken.dest.buffer = buffer;
    }
}

unsafe impl BufMut for OperationAllocBuffer {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.buffer.capacity() - self.buffer.len()
    }

    #[inline]
    unsafe fn advance_mut(&mut self, cnt: usize) {
        let len = self.buffer.len();
        let remaining = self.buffer.capacity() - len;

        assert!(cnt <= remaining, "cannot advance beyond capacity");

        self.buffer.set_len(len + cnt);
    }

    #[inline]
    fn chunk_mut(&mut self) -> &mut bytes::buf::UninitSlice {
        let capacity = self.buffer.capacity();
        let len = self.buffer.len();
        unsafe { &mut UninitSlice::from_raw_parts_mut(self.buffer.as_mut_ptr(), capacity)[len..] }
    }
}

impl Deref for OperationAllocBuffer {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &Self::Target {
        &self.buffer
    }
}

impl DerefMut for OperationAllocBuffer {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.buffer
    }
}
