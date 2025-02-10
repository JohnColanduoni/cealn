use std::io;

use futures::prelude::*;

use compio_core::buffer::{BufferRequest, RawInputBuffer, RawOutputBuffer};
use compio_internal_util::{input_buffer_passthrough_impl, output_buffer_passthrough_impl};

use crate::platform;

pub struct MessageChannel {
    pub(crate) imp: platform::message_channel::MessageChannel,
}

pub struct IdealOutputBuffer {
    pub(crate) imp: platform::message_channel::IdealOutputBuffer,
}

pub struct IdealInputBuffer {
    pub(crate) imp: platform::message_channel::IdealInputBuffer,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ReceiveResult {
    /// Indicates the message was read in its entirety to the buffer
    Full(usize),
    /// Indicates the message was too large to fit into the provided buffer, and a portion was truncated.
    Partial { bytes_read: usize, total_size: usize },
}

impl MessageChannel {
    #[inline]
    pub fn pair() -> io::Result<(MessageChannel, MessageChannel)> {
        let (a, b) = platform::message_channel::MessageChannel::pair()?;
        Ok((MessageChannel { imp: a }, MessageChannel { imp: b }))
    }

    #[inline]
    pub fn send<'a>(&'a self, buffer: impl RawOutputBuffer + 'a) -> impl Future<Output = io::Result<()>> + Send + 'a {
        self.imp.send(buffer)
    }

    /// Receives one packet from the other side of this `MessageChannel`
    ///
    /// If the buffer is too small to accomodate the message, it will be truncated and
    /// [`ReceiveResult::Partial`](self::ReceiveResult) will be returned.
    #[inline]
    pub fn recv<'a>(
        &'a self,
        buffer: impl RawInputBuffer + 'a,
    ) -> impl Future<Output = io::Result<ReceiveResult>> + Send + 'a {
        self.imp.recv(buffer)
    }

    /// Creates an [`RawOutputBuffer`](compio_core::buffer::RawOutputBuffer) that allows for the most efficient operation with
    /// [`send`](self::MessageChannel::send) calls.
    #[inline]
    pub fn new_output_buffer(&self, capacity: usize) -> io::Result<IdealOutputBuffer> {
        let imp = self.imp.new_output_buffer_with_traits(BufferRequest::new(capacity))?;
        Ok(IdealOutputBuffer { imp })
    }

    /// Creates an [`RawOutputBuffer`](compio_core::buffer::RawOutputBuffer) that allows for the most efficient operation with
    /// [`send`](self::MessageChannel::send) calls.
    #[inline]
    pub fn new_output_buffer_with_traits(&self, request: BufferRequest) -> io::Result<IdealOutputBuffer> {
        let imp = self.imp.new_output_buffer_with_traits(request)?;
        Ok(IdealOutputBuffer { imp })
    }

    /// Creates an [`RawInputBuffer`](compio_core::buffer::RawOutputBuffer) that allows for the most efficient operation with
    /// [`recv`](self::MessageChannel::recv) calls.
    #[inline]
    pub fn new_input_buffer(&self, capacity: usize) -> io::Result<IdealInputBuffer> {
        let imp = self.imp.new_input_buffer_with_traits(BufferRequest::new(capacity))?;
        Ok(IdealInputBuffer { imp })
    }

    /// Creates an [`RawInputBuffer`](compio_core::buffer::RawOutputBuffer) that allows for the most efficient operation with
    /// [`recv`](self::MessageChannel::recv) calls.
    #[inline]
    pub fn new_input_buffer_with_traits(&self, request: BufferRequest) -> io::Result<IdealInputBuffer> {
        let imp = self.imp.new_input_buffer_with_traits(request)?;
        Ok(IdealInputBuffer { imp })
    }
}

output_buffer_passthrough_impl!(IdealOutputBuffer => imp: platform::message_channel::IdealOutputBuffer);
input_buffer_passthrough_impl!(IdealOutputBuffer => imp: platform::message_channel::IdealOutputBuffer);

output_buffer_passthrough_impl!(IdealInputBuffer => imp: platform::message_channel::IdealInputBuffer);
input_buffer_passthrough_impl!(IdealInputBuffer => imp: platform::message_channel::IdealInputBuffer);

#[cfg(test)]
mod tests {
    use std::mem;

    use super::*;

    use bytes::{Buf, BufMut};
    use futures::join;

    use compio_core::buffer::{InputBuffer, OutputBuffer};
    use compio_executor::LocalPool;

    #[test]
    fn echo() {
        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let (mut a, mut b) = MessageChannel::pair().unwrap();

            let server = async move {
                loop {
                    let mut input_buffer = a.new_input_buffer(4096).unwrap();
                    a.recv(&mut input_buffer).await.unwrap();

                    if input_buffer_eq(&input_buffer, b"done") {
                        break;
                    }

                    a.send(&mut input_buffer).await.unwrap();
                }
            };

            let client = async move {
                let mut output_buffer = b.new_output_buffer(4096).unwrap();
                let mut input_buffer = b.new_input_buffer(4096).unwrap();
                output_buffer.put_slice(b"yo");
                b.send(&mut output_buffer).await.unwrap();
                b.recv(&mut input_buffer).await.unwrap();
                assert!(input_buffer_eq(&input_buffer, b"yo"));

                input_buffer.clear();
                b.send(&mut output_buffer).await.unwrap();
                b.recv(&mut input_buffer).await.unwrap();
                assert!(input_buffer_eq(&input_buffer, b"yo"));

                output_buffer.clear();
                output_buffer.put_slice(b"done");
                b.send(&mut output_buffer).await.unwrap();
            };

            join!(client, server);
        });
    }

    fn input_buffer_eq<I>(buffer: &I, expected: &[u8]) -> bool
    where
        I: InputBuffer,
        for<'a> &'a mut I: RawInputBuffer,
    {
        let mut remaining_expected = expected;
        let mut cursor = buffer.cursor();
        while cursor.has_remaining() {
            let cursor_chunk = cursor.chunk();
            let chunk_len = cursor_chunk.len();
            if remaining_expected.len() < chunk_len {
                return false;
            }
            let (head, tail) = remaining_expected.split_at(chunk_len);
            if head != cursor_chunk {
                return false;
            }
            remaining_expected = tail;
            cursor.advance(chunk_len);
        }

        remaining_expected.is_empty()
    }

    #[test]
    fn read_eof() {
        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let (a, b) = MessageChannel::pair().unwrap();

            let server = async move {
                let mut input_buffer = a.new_input_buffer(4096).unwrap();
                a.recv(&mut input_buffer).await.unwrap();

                assert_eq!(input_buffer.len(), 0);
            };

            let client = async move {
                mem::drop(b);
            };

            join!(client, server);
        });
    }
}
