use std::io::IoSliceMut;

use crate::buffer::RawInputBuffer;

pub struct UnpinnedIoVecMut<'s, 'd>(pub &'s mut [IoSliceMut<'d>]);

impl<'s, 'd> RawInputBuffer for UnpinnedIoVecMut<'s, 'd> {
    type Taken = Self;

    fn take(self) -> Self::Taken {
        self
    }

    fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
    where
        V: crate::buffer::InputBufferVisitor,
    {
        unsafe {
            visitor.unpinned_iovec(std::slice::from_raw_parts(
                taken.0.as_ptr() as *const libc::iovec,
                taken.0.len(),
            ))
        }
    }

    fn finalize(_taken: Self::Taken, _bytes_read: usize) {}
}
