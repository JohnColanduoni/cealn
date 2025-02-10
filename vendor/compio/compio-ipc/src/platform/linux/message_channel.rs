use std::{
    convert::TryFrom,
    ffi::OsString,
    io,
    mem::{self, MaybeUninit},
    os::{raw::c_int, unix::prelude::RawFd},
    ptr,
};

use libc;

use compio_core::{
    buffer::{BufferRequest, InputBufferVisitor, OutputBufferVisitor, RawInputBuffer, RawOutputBuffer},
    os::linux::Poller,
};
use compio_internal_util::{
    libc_call,
    unix::{set_nonblocking, ScopedFd},
};
use tracing::{debug, error, trace, trace_span};

use crate::message_channel::ReceiveResult;

pub use compio_core::buffer::AllocBuffer as IdealInputBuffer;

pub use compio_core::buffer::AllocBuffer as IdealOutputBuffer;

pub struct MessageChannel {
    socket: ScopedFd,
    operation: OperationProvider,
}

enum OperationProvider {
    Poll(Poller),
}

pub trait MessageChannelExt: Sized {
    /// Creates a [`MessageChannel`](crate::MessageChannel) from an existing socket
    ///
    /// The socket must be an `AF_UNIX`, `SOCK_SEQPACKET` socket
    unsafe fn from_socket(socket: RawFd) -> io::Result<Self>;
}

impl MessageChannel {
    pub fn pair() -> io::Result<(Self, Self)> {
        unsafe {
            let mut sockets: [c_int; 2] = [0; 2];
            libc_call!(libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                sockets.as_mut_ptr()
            ))?;
            let a = ScopedFd::from_raw(sockets[0]);
            let b = ScopedFd::from_raw(sockets[1]);
            let a = Self::from_fd(a)?;
            let b = Self::from_fd(b)?;
            Ok((a, b))
        }
    }

    fn from_fd(socket: ScopedFd) -> io::Result<Self> {
        // TODO: io_uring implementation with Send/Recv
        unsafe { set_nonblocking(socket.as_raw())? };
        let poller = unsafe { Poller::new(socket.as_raw())? };
        let operation = OperationProvider::Poll(poller);

        Ok(MessageChannel { socket, operation })
    }

    pub async fn send<B>(&self, buffer: B) -> io::Result<()>
    where
        B: RawOutputBuffer,
    {
        match &self.operation {
            OperationProvider::Poll(poller) => {
                let mut buffer = buffer.take();
                loop {
                    match B::visit(
                        &mut buffer,
                        SendVisitor {
                            socket: self.socket.as_raw(),
                        },
                    ) {
                        Ok(()) => return Ok(()),
                        Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                            poller.wait_for_write().await?;
                        }
                        Err(err) => return Err(err),
                    }
                }
            }
        }
    }

    pub async fn recv<B>(&self, buffer: B) -> io::Result<ReceiveResult>
    where
        B: RawInputBuffer,
    {
        match &self.operation {
            OperationProvider::Poll(poller) => {
                let mut buffer = buffer.take();
                loop {
                    match B::visit(
                        &mut buffer,
                        RecvVisitor {
                            socket: self.socket.as_raw(),
                        },
                    ) {
                        Ok(res @ ReceiveResult::Full(len)) => {
                            B::finalize(buffer, len);
                            return Ok(res);
                        }
                        Ok(res @ ReceiveResult::Partial { bytes_read, .. }) => {
                            B::finalize(buffer, bytes_read);
                            return Ok(res);
                        }
                        Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                            poller.wait_for_read().await?;
                        }
                        Err(err) => return Err(err),
                    }
                }
            }
        }
    }

    #[inline]
    pub fn new_output_buffer_with_traits(&self, request: BufferRequest) -> io::Result<IdealOutputBuffer> {
        Ok(IdealOutputBuffer::new(request))
    }

    #[inline]
    pub fn new_input_buffer_with_traits(&self, request: BufferRequest) -> io::Result<IdealOutputBuffer> {
        Ok(IdealInputBuffer::new(request))
    }
}

impl MessageChannelExt for crate::MessageChannel {
    unsafe fn from_socket(socket: RawFd) -> io::Result<Self> {
        Ok(crate::MessageChannel {
            imp: MessageChannel::from_fd(ScopedFd::from_raw(socket))?,
        })
    }
}

struct SendVisitor {
    socket: RawFd,
}

impl OutputBufferVisitor for SendVisitor {
    type Output = io::Result<()>;

    #[inline]
    fn unpinned_slice(self, data: &[u8]) -> Self::Output {
        unsafe {
            loop {
                let span = trace_span!("send");
                let _guard = span.enter();
                let ret = libc::send(self.socket, data.as_ptr() as _, data.len(), libc::MSG_NOSIGNAL);
                if ret < 0 {
                    let err = ::std::io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::WouldBlock {
                        // WouldBlock is expected here, so don't print an error for it
                        trace!("send returned WouldBlock");
                    } else if err.kind() == io::ErrorKind::Interrupted {
                        debug!("send was interrupted, retrying");
                        continue;
                    } else {
                        error!(
                            what = "libc_error",
                            function = "send",
                            error_code = ?err.raw_os_error(),
                            "function {} failed: {}", "send", err
                        );
                    }
                    return Err(err);
                }
                break;
            }

            Ok(())
        }
    }

    #[inline]
    fn unpinned_vector(self, data: &[&[u8]]) -> Self::Output {
        todo!()
    }
}

struct RecvVisitor {
    socket: RawFd,
}

impl InputBufferVisitor for RecvVisitor {
    type Output = io::Result<ReceiveResult>;

    #[inline]
    fn unpinned_slice(self, data: &mut [MaybeUninit<u8>]) -> Self::Output {
        unsafe {
            let ret = loop {
                let span = trace_span!("recv");
                let _guard = span.enter();
                let ret = libc::recv(self.socket, data.as_mut_ptr() as _, data.len(), libc::MSG_TRUNC);
                if ret < 0 {
                    let err = ::std::io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::WouldBlock {
                        // WouldBlock is expected here, so don't print an error for it
                        trace!("recv returned WouldBlock");
                    } else if err.kind() == io::ErrorKind::Interrupted {
                        debug!("recv was interrupted, retrying");
                        continue;
                    } else {
                        error!(
                            what = "libc_error",
                            function = "recv",
                            error_code = ?err.raw_os_error(),
                            "function {} failed: {}", "recv", err
                        );
                    }
                    return Err(err);
                }
                break ret;
            };

            // Check if message was truncated
            let actual_message_len = ret as usize;
            if actual_message_len > data.len() {
                Ok(ReceiveResult::Partial {
                    bytes_read: data.len(),
                    total_size: actual_message_len,
                })
            } else {
                Ok(ReceiveResult::Full(actual_message_len))
            }
        }
    }

    #[inline]
    fn unpinned_vector(self, data: &[&mut [MaybeUninit<u8>]]) -> Self::Output {
        todo!()
    }
}
