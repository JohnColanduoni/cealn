use std::{
    mem::{self, MaybeUninit},
    ops::{Bound, RangeBounds},
    slice::{self, SliceIndex},
};

use crate::buffer::{
    AllowCopy, AllowTake, InputBufferVisitor, OutputBufferVisitor, RawInputBuffer, RawOutputBuffer,
    SliceableRawOutputBuffer,
};

impl<'a> RawOutputBuffer for AllowCopy<&'a [u8]> {
    type Taken = &'a [u8];

    #[inline]
    fn take(self) -> Self::Taken {
        self.0
    }

    #[inline]
    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: OutputBufferVisitor,
    {
        visitor.unpinned_slice(taken)
    }
}

impl<'a> SliceableRawOutputBuffer for AllowCopy<&'a [u8]> {
    type Slice<'b> = AllowCopy<&'a [u8]> where 'a: 'b;

    #[inline]
    fn len(taken: &mut Self::Taken) -> usize {
        taken.len()
    }

    #[inline]
    fn slice<'b, R>(taken: &'b mut Self::Taken, range: R) -> Self::Slice<'b>
    where
        R: RangeBounds<usize>,
    {
        AllowCopy(index_slice_by_range_bounds(taken, range))
    }
}

impl<'a> RawInputBuffer for AllowCopy<&'a mut [u8]> {
    type Taken = &'a mut [u8];

    #[inline]
    fn take(self) -> Self::Taken {
        self.0
    }

    #[inline]
    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: InputBufferVisitor,
    {
        visitor.unpinned_slice(unsafe {
            slice::from_raw_parts_mut(taken.as_mut_ptr() as *mut MaybeUninit<u8>, taken.len())
        })
    }

    #[inline]
    fn finalize(_taken: Self::Taken, _bytes_read: usize) {}
}

pub struct TakenVecOutputBuffer<'a> {
    dest: Option<&'a mut Vec<u8>>,
    buffer: Vec<u8>,
}

impl<'a> Drop for TakenVecOutputBuffer<'a> {
    fn drop(&mut self) {
        if let Some(dest) = self.dest.as_deref_mut() {
            mem::swap(dest, &mut self.buffer)
        }
    }
}

impl<'a> RawOutputBuffer for AllowTake<&'a mut Vec<u8>> {
    type Taken = TakenVecOutputBuffer<'a>;

    #[inline]
    fn take(self) -> Self::Taken {
        let buffer = mem::replace(self.0, Vec::new());
        TakenVecOutputBuffer {
            dest: Some(self.0),
            buffer,
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

impl RawOutputBuffer for Vec<u8> {
    type Taken = TakenVecOutputBuffer<'static>;

    #[inline]
    fn take(self) -> Self::Taken {
        TakenVecOutputBuffer {
            dest: None,
            buffer: self,
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

pub struct TakenVecOutputBufferSlice<'b> {
    slice: &'b [u8],
}

impl<'a> SliceableRawOutputBuffer for AllowTake<&'a mut Vec<u8>> {
    type Slice<'b> = TakenVecOutputBufferSlice<'b> where 'a: 'b;

    #[inline]
    fn len(taken: &mut Self::Taken) -> usize {
        taken.buffer.len()
    }

    #[inline]
    fn slice<'b, R>(taken: &'b mut Self::Taken, range: R) -> Self::Slice<'b>
    where
        R: RangeBounds<usize>,
    {
        TakenVecOutputBufferSlice {
            slice: index_slice_by_range_bounds(&taken.buffer, range),
        }
    }
}

impl<'a> SliceableRawOutputBuffer for Vec<u8> {
    type Slice<'b> = TakenVecOutputBufferSlice<'b>;

    #[inline]
    fn len(taken: &mut Self::Taken) -> usize {
        taken.buffer.len()
    }

    #[inline]
    fn slice<'b, R>(taken: &'b mut Self::Taken, range: R) -> Self::Slice<'b>
    where
        R: RangeBounds<usize>,
    {
        TakenVecOutputBufferSlice {
            slice: index_slice_by_range_bounds(&taken.buffer, range),
        }
    }
}

impl<'b> RawOutputBuffer for TakenVecOutputBufferSlice<'b> {
    type Taken = Self;

    #[inline]
    fn take(self) -> Self::Taken {
        self
    }

    #[inline]
    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: OutputBufferVisitor,
    {
        unsafe { visitor.pinned_slice(taken.slice) }
    }
}

/// Appends to the end of the [`Vec`](std::vec::Vec), using excess capacity as the input buffer
impl<'a> RawInputBuffer for AllowTake<&'a mut Vec<u8>> {
    type Taken = TakenVecInputBuffer<'a>;

    #[inline]
    fn take(self) -> Self::Taken {
        let buffer = mem::replace(self.0, Vec::new());
        TakenVecInputBuffer {
            dest: self.0,
            buffer: Some(buffer),
        }
    }

    #[inline]
    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: InputBufferVisitor,
    {
        unsafe {
            let buffer = taken.buffer.as_mut().expect("already finalized");
            visitor.pinned_slice(slice::from_raw_parts_mut(
                buffer.as_mut_ptr().add(buffer.len()) as *mut MaybeUninit<u8>,
                buffer.capacity() - buffer.len(),
            ))
        }
    }

    #[inline]
    fn finalize(mut taken: Self::Taken, bytes_read: usize) {
        unsafe {
            let mut buffer = taken.buffer.take().expect("already finalized");
            assert!(bytes_read <= buffer.capacity() - buffer.len());
            buffer.set_len(buffer.len() + bytes_read);
            *taken.dest = buffer;
        }
    }
}

pub struct TakenVecInputBuffer<'a> {
    dest: &'a mut Vec<u8>,
    buffer: Option<Vec<u8>>,
}

impl<'a> Drop for TakenVecInputBuffer<'a> {
    fn drop(&mut self) {
        if let Some(buffer) = self.buffer.take() {
            // `finalize` was never called, so put the buffer back with its original length
            *self.dest = buffer;
        }
    }
}

impl RawOutputBuffer for &'static [u8] {
    type Taken = &'static [u8];

    #[inline]
    fn take(self) -> Self::Taken {
        self
    }

    #[inline]
    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: OutputBufferVisitor,
    {
        unsafe { visitor.pinned_slice(*taken) }
    }
}

impl<'a> RawOutputBuffer for &'a bytes::Bytes {
    type Taken = bytes::Bytes;

    fn take(self) -> Self::Taken {
        self.clone()
    }

    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: OutputBufferVisitor,
    {
        unsafe { visitor.pinned_slice(taken) }
    }
}

impl<'a> RawOutputBuffer for bytes::Bytes {
    type Taken = bytes::Bytes;

    fn take(self) -> Self::Taken {
        self
    }

    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: OutputBufferVisitor,
    {
        unsafe { visitor.pinned_slice(taken) }
    }
}

impl<'a> SliceableRawOutputBuffer for &'a bytes::Bytes {
    type Slice<'b> = bytes::Bytes where 'a: 'b;

    #[inline]
    fn len(taken: &mut Self::Taken) -> usize {
        taken.len()
    }

    #[inline]
    fn slice<'b, R>(taken: &'b mut Self::Taken, range: R) -> bytes::Bytes
    where
        R: RangeBounds<usize>,
    {
        taken.slice(range)
    }
}

impl SliceableRawOutputBuffer for bytes::Bytes {
    type Slice<'b> = bytes::Bytes;

    #[inline]
    fn len(taken: &mut Self::Taken) -> usize {
        taken.len()
    }

    #[inline]
    fn slice<'b, R>(taken: &'b mut Self::Taken, range: R) -> bytes::Bytes
    where
        R: RangeBounds<usize>,
    {
        taken.slice(range)
    }
}

impl<'a> RawInputBuffer for &'a mut bytes::BytesMut {
    type Taken = TakenBytesMut<'a>;

    #[inline]
    fn take(self) -> Self::Taken {
        TakenBytesMut {
            spare_capacity: self.split_off(self.len()),
            dest: self,
        }
    }

    #[inline]
    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: InputBufferVisitor,
    {
        unsafe { visitor.pinned_slice(taken.spare_capacity.spare_capacity_mut()) }
    }

    #[inline]
    fn finalize(mut taken: Self::Taken, bytes_read: usize) {
        unsafe {
            taken.spare_capacity.set_len(bytes_read);
            taken.dest.unsplit(taken.spare_capacity);
        }
    }
}

pub struct TakenBytesMut<'a> {
    dest: &'a mut bytes::BytesMut,
    spare_capacity: bytes::BytesMut,
}

#[inline]
fn index_slice_by_range_bounds<T, R>(slice: &[T], range: R) -> &[T]
where
    R: RangeBounds<usize>,
{
    match (range.start_bound(), range.end_bound()) {
        (Bound::Included(&a), Bound::Included(&b)) => &slice[a..=b],
        (Bound::Included(&a), Bound::Excluded(&b)) => &slice[a..b],
        (Bound::Included(&a), Bound::Unbounded) => &slice[a..],
        (Bound::Excluded(&a), Bound::Included(&b)) => &slice[(a + 1)..=b],
        (Bound::Excluded(&a), Bound::Excluded(&b)) => &slice[(a + 1)..b],
        (Bound::Excluded(&a), Bound::Unbounded) => &slice[(a + 1)..],
        (Bound::Unbounded, Bound::Included(&b)) => &slice[..=b],
        (Bound::Unbounded, Bound::Excluded(&b)) => &slice[..b],
        (Bound::Unbounded, Bound::Unbounded) => &slice[..],
    }
}
