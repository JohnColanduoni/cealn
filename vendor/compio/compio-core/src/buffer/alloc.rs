use std::{
    alloc::{alloc, alloc_zeroed, dealloc, Layout},
    mem::{self, ManuallyDrop, MaybeUninit},
    ops::{Deref, DerefMut},
    ptr::NonNull,
    slice,
};

use bytes::{buf::UninitSlice, BufMut};

use crate::buffer::{InputBuffer, OutputBuffer, OutputBufferVisitor, RawInputBuffer, RawOutputBuffer};

use super::BufferRequest;

/// An allocated buffer that allows more detailed control over properties of the allocation
///
/// Behaves the same as a [`Vec`](std::vec::Vec), but allows modulating more paramteters of the allocation.
pub struct AllocBuffer {
    ptr: NonNull<u8>,
    len: usize,
    layout: Layout,
}

unsafe impl Send for AllocBuffer {}
unsafe impl Sync for AllocBuffer {}

impl Drop for AllocBuffer {
    fn drop(&mut self) {
        unsafe {
            if self.layout.size() > 0 {
                dealloc(self.ptr.as_ptr(), self.layout)
            }
        }
    }
}

impl AllocBuffer {
    #[inline]
    pub fn new(request: BufferRequest) -> Self {
        unsafe {
            let layout =
                Layout::from_size_align(request.capacity, request.alignment.unwrap_or(1)).expect("invalid layout");

            let ptr = if request.zeroed {
                alloc_zeroed(layout)
            } else {
                alloc(layout)
            };

            AllocBuffer {
                ptr: NonNull::new_unchecked(ptr),
                len: 0,
                layout,
            }
        }
    }

    #[inline]
    pub fn empty() -> Self {
        unsafe {
            AllocBuffer {
                ptr: NonNull::dangling(),
                len: 0,
                layout: Layout::from_size_align_unchecked(0, 1),
            }
        }
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    #[inline]
    pub fn capacity(&self) -> usize {
        self.layout.size()
    }

    #[inline]
    pub unsafe fn set_len(&mut self, new_len: usize) {
        debug_assert!(new_len <= self.capacity());
        self.len = new_len;
    }

    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }
}

impl OutputBuffer for AllocBuffer {
    #[inline]
    fn clear(&mut self) {
        AllocBuffer::clear(self)
    }

    #[inline]
    fn as_contiguous(&self) -> Option<&[u8]> {
        Some(&self)
    }

    #[inline]
    fn as_contiguous_mut(&mut self) -> Option<&mut [u8]> {
        Some(self)
    }
}

impl InputBuffer for AllocBuffer {
    #[inline]
    fn len(&self) -> usize {
        AllocBuffer::len(self)
    }

    #[inline]
    fn chunk_at(&self, offset: usize) -> &[u8] {
        &self[offset..]
    }

    #[inline]
    fn as_contiguous(&self) -> Option<&[u8]> {
        Some(&self)
    }
}

pub struct TakenAllocOutputBuffer<'a> {
    dest: &'a mut AllocBuffer,
    buffer: ManuallyDrop<AllocBuffer>,
}

impl<'a> Drop for TakenAllocOutputBuffer<'a> {
    fn drop(&mut self) {
        unsafe {
            // This is safe because we only ever move `buffer` here (and don't access it afterwards)
            *self.dest = ManuallyDrop::take(&mut self.buffer);
        }
    }
}

impl<'a> RawOutputBuffer for &'a mut AllocBuffer {
    type Taken = TakenAllocOutputBuffer<'a>;

    #[inline]
    fn take(self) -> Self::Taken {
        let buffer = mem::replace(self, AllocBuffer::empty());
        TakenAllocOutputBuffer {
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

pub struct TakenAllocInputBuffer<'a> {
    dest: &'a mut AllocBuffer,
    buffer: Option<AllocBuffer>,
}

impl<'a> Drop for TakenAllocInputBuffer<'a> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            // Buffer was never finalized, just put it back
            *self.dest = buffer;
        }
    }
}

impl<'a> RawInputBuffer for &'a mut AllocBuffer {
    type Taken = TakenAllocInputBuffer<'a>;

    #[inline]
    fn take(self) -> Self::Taken {
        let buffer = mem::replace(self, AllocBuffer::empty());
        TakenAllocInputBuffer {
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

        *taken.dest = buffer;
    }
}

unsafe impl BufMut for AllocBuffer {
    #[inline]
    fn remaining_mut(&self) -> usize {
        self.capacity() - self.len()
    }

    #[inline]
    unsafe fn advance_mut(&mut self, cnt: usize) {
        let len = self.len();
        let remaining = self.capacity() - len;

        assert!(cnt <= remaining, "cannot advance beyond capacity");

        self.set_len(len + cnt);
    }

    #[inline]
    fn chunk_mut(&mut self) -> &mut bytes::buf::UninitSlice {
        let capacity = self.capacity();
        let len = self.len();
        unsafe { &mut UninitSlice::from_raw_parts_mut(self.as_mut_ptr(), capacity)[len..] }
    }
}

impl Deref for AllocBuffer {
    type Target = [u8];

    #[inline]
    fn deref(&self) -> &[u8] {
        unsafe { slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }
}

impl DerefMut for AllocBuffer {
    #[inline]
    fn deref_mut(&mut self) -> &mut [u8] {
        unsafe { slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }
}

impl AsRef<[u8]> for AllocBuffer {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        &self
    }
}
