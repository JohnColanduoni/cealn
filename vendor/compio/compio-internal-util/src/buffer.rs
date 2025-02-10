#[macro_export]
macro_rules! output_buffer_passthrough_impl {
    ($wrapper_ty:ty => $member:ident : $inner_ty:ty) => {
        impl ::compio_core::buffer::OutputBuffer for $wrapper_ty {
            #[inline]
            fn clear(&mut self) {
                self.$member.clear();
            }

            #[inline]
            fn as_contiguous(&self) -> Option<&[u8]> {
                ::compio_core::buffer::OutputBuffer::as_contiguous(&self.$member)
            }

            #[inline]
            fn as_contiguous_mut(&mut self) -> Option<&mut [u8]> {
                ::compio_core::buffer::OutputBuffer::as_contiguous_mut(&mut self.$member)
            }
        }

        impl<'a> ::compio_core::buffer::RawOutputBuffer for &'a mut $wrapper_ty {
            type Taken = <&'a mut $inner_ty as ::compio_core::buffer::RawOutputBuffer>::Taken;

            #[inline]
            fn take(self) -> Self::Taken {
                ::compio_core::buffer::RawOutputBuffer::take(&mut self.$member)
            }

            #[inline]
            fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
            where
                V: ::compio_core::buffer::OutputBufferVisitor,
            {
                <&'a mut $inner_ty as ::compio_core::buffer::RawOutputBuffer>::visit(taken, visitor)
            }
        }

        unsafe impl ::bytes::BufMut for $wrapper_ty {
            #[inline]
            fn remaining_mut(&self) -> usize {
                self.imp.remaining_mut()
            }

            #[inline]
            unsafe fn advance_mut(&mut self, cnt: usize) {
                self.imp.advance_mut(cnt)
            }

            #[inline]
            fn chunk_mut(&mut self) -> &mut ::bytes::buf::UninitSlice {
                self.imp.chunk_mut()
            }
        }
    };
}

#[macro_export]
macro_rules! input_buffer_passthrough_impl {
    ($wrapper_ty:ty => $member:ident : $inner_ty:ty) => {
        impl ::compio_core::buffer::InputBuffer for $wrapper_ty {
            #[inline]
            fn len(&self) -> usize {
                ::compio_core::buffer::InputBuffer::len(&self.$member)
            }

            fn chunk_at(&self, offset: usize) -> &[u8] {
                ::compio_core::buffer::InputBuffer::chunk_at(&self.$member, offset)
            }

            fn as_contiguous(&self) -> Option<&[u8]> {
                ::compio_core::buffer::InputBuffer::as_contiguous(&self.$member)
            }
        }

        impl<'a> RawInputBuffer for &'a mut $wrapper_ty {
            type Taken = <&'a mut $inner_ty as ::compio_core::buffer::RawInputBuffer>::Taken;

            #[inline]
            fn take(self) -> Self::Taken {
                ::compio_core::buffer::RawInputBuffer::take(&mut self.$member)
            }

            #[inline]
            fn visit<V>(taken: &mut Self::Taken, visitor: V) -> V::Output
            where
                V: ::compio_core::buffer::InputBufferVisitor,
            {
                <&'a mut $inner_ty as ::compio_core::buffer::RawInputBuffer>::visit(taken, visitor)
            }

            #[inline]
            fn finalize(taken: Self::Taken, bytes_read: usize) {
                <&'a mut $inner_ty as ::compio_core::buffer::RawInputBuffer>::finalize(taken, bytes_read)
            }
        }
    };
}
