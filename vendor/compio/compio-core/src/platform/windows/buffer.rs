use smallvec::SmallVec;
use winapi::shared::ws2def::WSABUF;

use crate::buffer::OutputBufferVisitor;

#[inline]
pub unsafe fn output_buffer_visit_wsabuf_fallback<V>(visitor: V, bufs: *const WSABUF, buf_count: usize) -> V::Output
where
    V: OutputBufferVisitor,
{
    if buf_count == 1 {
        // No performance issue if there is only one buffer: we can just use the `pinned_slice` directly.
        let buf = *bufs;
        return visitor.pinned_slice(std::slice::from_raw_parts(
            buf.buf as *mut u8 as *const u8,
            buf.len as usize,
        ));
    }

    #[cfg(feature = "perf-warnings")]
    tracing::warn!(
        "attempted to use wsabuf with an operation that doesn't support it. a copy of the buffer pointers was made."
    );

    let buf_slice = std::slice::from_raw_parts(bufs, buf_count);
    let mut buffers: SmallVec<[&[u8]; 8]> = SmallVec::with_capacity(buf_count);
    for buf in buf_slice {
        buffers.push(std::slice::from_raw_parts(
            buf.buf as *mut u8 as *const u8,
            buf.len as usize,
        ));
    }
    visitor.pinned_vector(&buffers)
}
