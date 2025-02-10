pub(crate) mod alloc;
mod imp;
mod utils;

pub use self::{
    alloc::AllocBuffer,
    utils::{ensure_pinned_input, ensure_pinned_output, PinnedInputGuard, PinnedOutputGuard},
};

use std::{
    mem::MaybeUninit,
    ops::{Range, RangeBounds},
    slice::SliceIndex,
};

use bytes::{Buf, BufMut};

pub trait OutputBuffer: BufMut + Send
where
    for<'a> &'a mut Self: RawOutputBuffer,
{
    /// Resets the region of the buffer that is marked as allocated to empty, allowing reuse of the buffer
    fn clear(&mut self);

    /// If the underlying buffer is contiguous, returns its initialized extent
    ///
    /// In general this may fail. Callers should explicitly request a contiguous buffer with
    /// [`BufferRequest.contiguous`](crate::buffer::BufferRequest) if they absolutely need one.
    fn as_contiguous(&self) -> Option<&[u8]>;

    /// If the underlying buffer is contiguous, returns its initialized extent
    ///
    /// In general this may fail. Callers should explicitly request a contiguous buffer with
    /// [`BufferRequest.contiguous`](crate::buffer::BufferRequest) if they absolutely need one.
    fn as_contiguous_mut(&mut self) -> Option<&mut [u8]>;
}

pub trait InputBuffer: Send
where
    for<'a> &'a mut Self: RawInputBuffer,
{
    fn len(&self) -> usize;

    /// Gets the contiguous chunk of the buffer starting at the designated offset
    fn chunk_at(&self, offset: usize) -> &[u8];

    /// If the underlying buffer is contiguous, returns its initialized extent
    ///
    /// In general this may fail. Callers should explicitly request a contiguous buffer with
    /// [`BufferRequest.contiguous`](crate::buffer::BufferRequest) if they absolutely need one.
    fn as_contiguous(&self) -> Option<&[u8]>;

    #[inline]
    fn cursor(&self) -> Cursor<&Self> {
        Cursor {
            inner: self,
            position: 0,
        }
    }
}

/// A cursor for an [`InputBuffer`](self::InputBuffer)
// We don't use `std::io::Cursor` because we can't implement foreign traits (specifically `Buf`) for it
pub struct Cursor<T> {
    inner: T,
    position: usize,
}

/// An output buffer usable by IO operations
///
/// This type allows IO operations to negotiate the ideal way to use a particular buffer format. This includes generic
/// unpinned and pinned buffers as well as platform/backend specific buffer types. Most IO primitives will offer to
/// provide implementations of the more abstract [`OutputBuffer`](self::OutputBuffer) trait that will automatically
/// help you write data in the ideal format.
pub trait RawOutputBuffer: Send {
    type Taken: Send;

    /// Obtains ownership of the buffer that ensures the buffer is pinned until the operation is complete (if relevant)
    fn take(self) -> Self::Taken;

    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: OutputBufferVisitor;
}

pub trait SliceableRawOutputBuffer: RawOutputBuffer {
    type Slice<'a>: RawOutputBuffer + 'a
    where
        Self: 'a;

    fn len(taken: &mut Self::Taken) -> usize;

    fn slice<'a, R>(taken: &'a mut Self::Taken, range: R) -> Self::Slice<'a>
    where
        R: RangeBounds<usize>;
}

/// An input buffer usable by IO operations
///
/// This type allows IO operations to negotiate the ideal way to use a particular buffer format. This includes generic
/// unpinned and pinned buffers as well as platform/backend specific buffer types. Most IO primitives will offer to
/// provide implementations of the more abstract [`InputBuffer`](self::InputBuffer) trait that will automatically
/// help you write data in the ideal format.
pub trait RawInputBuffer: Send {
    type Taken: Send;

    /// Obtains ownership of the buffer that ensures the buffer is pinned until the operation is complete (if relevant)
    fn take(self) -> Self::Taken;

    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: InputBufferVisitor;

    /// Inform buffer of the number of bytes written and release it back to the caller
    fn finalize(taken: Self::Taken, bytes_read: usize);
}

/// An implementation of an operation on a generic buffer
///
/// Operations implement this type to allow using the most efficient method for each buffer/operation combination. Only
/// the basic `unpinned_slice` and `unpinned_vector` are required, but the pinned and platform-specific variants may be
/// additionally implemented if a more efficient mapping is possible.
pub trait OutputBufferVisitor: Sized {
    type Output;

    fn unpinned_slice(self, data: &[u8]) -> Self::Output;

    fn unpinned_vector(self, data: &[&[u8]]) -> Self::Output;

    #[inline]
    unsafe fn pinned_slice(self, data: &[u8]) -> Self::Output {
        self.unpinned_slice(data)
    }

    #[inline]
    unsafe fn pinned_vector(self, data: &[&[u8]]) -> Self::Output {
        self.unpinned_vector(data)
    }

    #[cfg(unix)]
    unsafe fn unpinned_iovec(self, data: &[libc::iovec]) -> Self::Output {
        todo!()
    }

    #[cfg(unix)]
    unsafe fn pinned_iovec(self, data: &[libc::iovec]) -> Self::Output {
        todo!()
    }

    #[cfg(target_os = "windows")]
    #[inline]
    unsafe fn win32_file_segment_array(self, _data: *const winapi::um::winnt::FILE_SEGMENT_ELEMENT) -> Self::Output {
        todo!()
    }

    #[cfg(target_os = "windows")]
    #[inline]
    unsafe fn wsabuf(self, bufs: *const winapi::shared::ws2def::WSABUF, buf_count: usize) -> Self::Output {
        crate::platform::buffer::output_buffer_visit_wsabuf_fallback(self, bufs, buf_count)
    }
}

const ALLOC_FREE_SLICE_CONVERT_LEN: usize = 16;

pub trait InputBufferVisitor: Sized {
    type Output;

    fn unpinned_slice(self, buffer: &mut [MaybeUninit<u8>]) -> Self::Output;

    fn unpinned_vector(self, buffer: &[&mut [MaybeUninit<u8>]]) -> Self::Output;

    #[inline]
    unsafe fn pinned_slice(self, buffer: &mut [MaybeUninit<u8>]) -> Self::Output {
        self.unpinned_slice(buffer)
    }

    #[inline]
    unsafe fn pinned_vector(self, buffer: &[&mut [MaybeUninit<u8>]]) -> Self::Output {
        self.unpinned_vector(buffer)
    }

    #[cfg(unix)]
    unsafe fn unpinned_iovec(self, data: &[libc::iovec]) -> Self::Output {
        use smallvec::SmallVec;

        if data.len() == 1 {
            let vec = &data[0];
            return self.unpinned_slice(std::slice::from_raw_parts_mut(
                vec.iov_base as *mut MaybeUninit<u8>,
                vec.iov_len,
            ));
        }

        let mut slices: SmallVec<[&mut [MaybeUninit<u8>]; ALLOC_FREE_SLICE_CONVERT_LEN]> = SmallVec::new();
        slices.extend(
            data.iter()
                .map(|vec| std::slice::from_raw_parts_mut(vec.iov_base as *mut MaybeUninit<u8>, vec.iov_len)),
        );
        self.unpinned_vector(&slices)
    }

    #[cfg(unix)]
    unsafe fn pinned_iovec(self, data: &[libc::iovec]) -> Self::Output {
        use smallvec::SmallVec;

        if data.len() == 1 {
            let vec = &data[0];
            return self.pinned_slice(std::slice::from_raw_parts_mut(
                vec.iov_base as *mut MaybeUninit<u8>,
                vec.iov_len,
            ));
        }

        let mut slices: SmallVec<[&mut [MaybeUninit<u8>]; ALLOC_FREE_SLICE_CONVERT_LEN]> = SmallVec::new();
        slices.extend(
            data.iter()
                .map(|vec| std::slice::from_raw_parts_mut(vec.iov_base as *mut MaybeUninit<u8>, vec.iov_len)),
        );
        self.pinned_vector(&slices)
    }

    #[cfg(target_os = "windows")]
    #[inline]
    unsafe fn win32_file_segment_array(self, _data: *const winapi::um::winnt::FILE_SEGMENT_ELEMENT) -> Self::Output {
        todo!()
    }

    #[cfg(target_os = "windows")]
    #[inline]
    unsafe fn wsabuf(self, _bufs: *const winapi::shared::ws2def::WSABUF, _buf_count: usize) -> Self::Output {
        todo!()
    }
}

/// Specifies common traits that an application may want from a buffer
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct BufferRequest {
    pub capacity: usize,
    pub alignment: Option<usize>,

    /// Indicates the buffer must consist of a single contiguous region of memory
    ///
    /// Defaults to `false`.
    pub contiguous: bool,

    /// Indicates the buffer must be initialized to zeroes
    ///
    /// Defaults to `false`.
    pub zeroed: bool,
}

impl BufferRequest {
    pub fn new(capacity: usize) -> BufferRequest {
        BufferRequest {
            capacity,
            alignment: None,
            contiguous: false,
            zeroed: false,
        }
    }
}

/// A wrapper that allows using buffer types that may require copies when used with certain backends (e.g. `&[u8]`)
///
/// This type exists to ensure callers know when they may have inadvertently passed a buffer in such a way that
/// zero-copy operation is not possible on certain backends. Usually this means the data was passed by borrow and so
/// native completion-based backends cannot ensure the buffer lives until the OS is done with it.
pub struct AllowCopy<T>(pub T);

/// A wrapper that allows using buffer types that may require modifying the original when used with certain backends (e.g. `&mut Vec<u8>`)
///
/// This wrapper doesn't exist to make a performance issue clear like [`AllowCopy`](self::AllowCopy), but instead to
/// avoid confusing the user when they e.g. try to pass a `&mut Vec<u8>` as a `&mut [u8]` but it empties their vector
/// instead.
///
/// When used for an I/O operation, the callee will ensure the value is put in a consistent state (e.g. empty `Vec`)
/// regardless of whether the operation succeeded or not.
pub struct AllowTake<T>(pub T);

impl<'a, T> Buf for Cursor<&'a T>
where
    T: InputBuffer,
    for<'b> &'b mut T: RawInputBuffer,
{
    #[inline]
    fn remaining(&self) -> usize {
        self.inner.len().saturating_sub(self.position)
    }

    #[inline]
    fn chunk(&self) -> &[u8] {
        self.inner.chunk_at(self.position)
    }

    #[inline]
    fn advance(&mut self, cnt: usize) {
        match self.position.checked_add(cnt) {
            Some(new_position) if new_position <= self.inner.len() => {
                self.position = new_position;
            }
            _ => panic!("attempted to advanced beyond end of initialized buffer region"),
        }
    }
}
