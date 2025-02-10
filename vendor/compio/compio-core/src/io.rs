use std::{
    cell::UnsafeCell,
    cmp, io,
    io::Result,
    marker::PhantomPinned,
    mem::{self, ManuallyDrop},
    pin::Pin,
    task::{Context, Poll},
};

use futures::{future::BoxFuture, prelude::*};
use pin_project::{pin_project, pinned_drop};

use crate::buffer::{AllowCopy, AllowTake, RawInputBuffer, RawOutputBuffer, SliceableRawOutputBuffer};

pub trait AsyncRead: Send + Sized {
    type Read<'a, I: RawInputBuffer + 'a>: Future<Output = Result<usize>> + Send + 'a
    where
        Self: 'a;

    fn read<'a, I>(&'a mut self, buffer: I) -> Self::Read<'a, I>
    where
        I: RawInputBuffer + 'a;

    #[inline]
    fn pollable_read<'a>(&'a mut self) -> PollableRead<'a, Self>
    where
        Self::Read<'a, AllowTake<&'static mut Vec<u8>>>: Sized,
    {
        PollableRead {
            imp: self,
            buffer: UnsafeCell::new(Vec::new()),
            operation: None,
            _phantom: PhantomPinned,
        }
    }
}

pub trait AsyncWrite: Send + Sized {
    type Write<'a, O: RawOutputBuffer + 'a>: Future<Output = Result<usize>> + Send + 'a
    where
        Self: 'a;

    fn write<'a, O>(&'a mut self, buffer: O) -> Self::Write<'a, O>
    where
        O: RawOutputBuffer + 'a;
}

pub trait AsyncReadExt {
    fn read_to_end<'a>(&'a mut self, buffer: AllowTake<&'a mut Vec<u8>>) -> BoxFuture<'a, Result<usize>>;
}

pub trait AsyncWriteExt {
    fn write_all<'a, O>(&'a mut self, buffer: O) -> BoxFuture<'a, Result<()>>
    where
        O: SliceableRawOutputBuffer + 'a;
}

impl<T: AsyncRead> AsyncReadExt for T
where
    T: 'static,
{
    fn read_to_end<'a>(&'a mut self, buffer: AllowTake<&'a mut Vec<u8>>) -> BoxFuture<'a, Result<usize>> {
        let fut = async move {
            let mut total_bytes_read = 0;
            loop {
                if buffer.0.len() == buffer.0.capacity() {
                    buffer.0.reserve(128 * 1024);
                }
                let bytes_read = self.read(AllowTake(&mut *buffer.0)).await?;
                if bytes_read == 0 {
                    break;
                }
                total_bytes_read += bytes_read;
            }
            Ok(total_bytes_read)
        };
        // FIXME: can't prove this is send with current compiler
        unsafe {
            let fut: Pin<Box<dyn Future<Output = Result<usize>>>> = Box::pin(fut);
            mem::transmute(fut)
        }
    }
}

impl<T: AsyncWrite> AsyncWriteExt for T
where
    T: 'static,
{
    fn write_all<'a, O>(&'a mut self, buffer: O) -> BoxFuture<'a, Result<()>>
    where
        O: SliceableRawOutputBuffer + Send + 'a,
    {
        let fut = async move {
            unsafe {
                // FIXME: release buffer eventually on cancelation
                let mut taken = ManuallyDrop::new(buffer.take());
                let result: Result<()> = try {
                    let mut offset = 0;
                    while offset < O::len(&mut taken) {
                        let bytes_written = self.write(O::slice(&mut taken, offset..)).await?;
                        if bytes_written == 0 {
                            ManuallyDrop::drop(&mut taken);
                            std::result::Result::<(), std::io::Error>::Err(std::io::ErrorKind::WriteZero.into())?;
                        }
                        offset += bytes_written;
                    }
                };
                ManuallyDrop::drop(&mut taken);
                result
            }
            // TODO: unbox
        };
        unsafe {
            let fut: Pin<Box<dyn Future<Output = Result<()>>>> = Box::pin(fut);
            mem::transmute(fut)
        }
    }
}

#[pin_project(PinnedDrop)]
pub struct PollableRead<'a, T>
where
    T: AsyncRead + 'a,
{
    imp: &'a mut T,
    #[pin]
    buffer: UnsafeCell<Vec<u8>>,
    #[pin]
    operation: Option<T::Read<'a, AllowTake<&'static mut Vec<u8>>>>,
    _phantom: PhantomPinned,
}

#[pinned_drop]
impl<'a, T> PinnedDrop for PollableRead<'a, T>
where
    T: AsyncRead + 'a,
{
    fn drop(self: Pin<&mut Self>) {
        let mut this = self.project();
        this.operation.set(None);
    }
}

unsafe impl<'a, T> Send for PollableRead<'a, T> where T: AsyncRead + Send + 'a {}
unsafe impl<'a, T> Sync for PollableRead<'a, T> where T: AsyncRead + Send + 'a {}

impl<'a, T> futures::io::AsyncRead for PollableRead<'a, T>
where
    T: AsyncRead,
{
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<Result<usize>> {
        let mut this = self.project();
        loop {
            match this.operation.as_mut().as_pin_mut() {
                Some(operation) => match operation.poll(cx) {
                    Poll::Ready(_) => unsafe {
                        let buffer = &mut *this.buffer.get();
                        let to_copy = cmp::min(buf.len(), buffer.len());
                        buf[..to_copy].copy_from_slice(&buffer[..to_copy]);
                        let _ = buffer.drain(..to_copy);
                        *this.operation.get_unchecked_mut() = None;
                        return Poll::Ready(Ok(to_copy));
                    },
                    Poll::Pending => return Poll::Pending,
                },
                None => unsafe {
                    {
                        let buffer = &mut *this.buffer.get();
                        if !buffer.is_empty() {
                            let to_copy = cmp::min(buf.len(), buffer.len());
                            buf[..to_copy].copy_from_slice(&buffer[..to_copy]);
                            let _ = buffer.drain(..to_copy);
                            return Poll::Ready(Ok(to_copy));
                        }

                        buffer.reserve_exact(buf.len());
                    }
                    let buffer_static: &'static mut Vec<u8> = &mut *this.buffer.get();
                    let imp_static: &'a mut T = mem::transmute::<&mut T, _>(&mut **this.imp);
                    let operation = imp_static.read(AllowTake(buffer_static));
                    *this.operation.as_mut().get_unchecked_mut() = Some(operation);
                    continue;
                },
            }
        }
    }

    // TODO: native vectored read
}

#[cfg(feature = "tokio")]
impl<'a, T> tokio::io::AsyncRead for PollableRead<'a, T>
where
    T: AsyncRead,
{
    fn poll_read(self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut tokio::io::ReadBuf<'_>) -> Poll<Result<()>> {
        let mut this = self.project();
        loop {
            match this.operation.as_mut().as_pin_mut() {
                Some(operation) => match operation.poll(cx) {
                    Poll::Ready(_) => unsafe {
                        let buffer = &mut *this.buffer.get();
                        let to_copy = cmp::min(buf.remaining(), buffer.len());
                        buf.put_slice(&buffer[..to_copy]);
                        let _ = buffer.drain(..to_copy);
                        *this.operation.get_unchecked_mut() = None;
                        return Poll::Ready(Ok(()));
                    },
                    Poll::Pending => return Poll::Pending,
                },
                None => unsafe {
                    {
                        let buffer = &mut *this.buffer.get();
                        if !buffer.is_empty() {
                            let to_copy = cmp::min(buf.remaining(), buffer.len());
                            buf.put_slice(&buffer[..to_copy]);
                            let _ = buffer.drain(..to_copy);
                            return Poll::Ready(Ok(()));
                        }

                        buffer.reserve_exact(buf.remaining());
                    }
                    let buffer_static: &'static mut Vec<u8> = &mut *this.buffer.get();
                    let imp_static: &'a mut T = mem::transmute::<&mut T, _>(&mut **this.imp);
                    let operation = imp_static.read(AllowTake(buffer_static));
                    this.operation.as_mut().set(Some(operation));
                    continue;
                },
            }
        }
    }
}

impl<'a, 'b, T> std::io::Read for Pin<&'b mut PollableRead<'a, T>>
where
    T: AsyncRead,
{
    fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        todo!()
    }
}
