use std::{
    io,
    mem::MaybeUninit,
    net::{SocketAddr, UdpSocket as StdUdpSocket},
    os::unix::prelude::{AsRawFd, RawFd},
    sync::Arc,
};

use compio_core::{
    buffer::{InputBufferVisitor, OutputBufferVisitor, RawInputBuffer, RawOutputBuffer},
    os::linux::*,
    EventQueue,
};

pub(crate) struct UdpSocket {
    std: Arc<StdUdpSocket>,
    operation: OperationProvider,
}

enum OperationProvider {
    Poll(Poller),
    #[cfg(feature = "io-uring")]
    IoUringSendRecv(super::io_uring::UdpSendRecv),
}

impl UdpSocket {
    pub(crate) fn bind(socket_addr: SocketAddr) -> io::Result<UdpSocket> {
        let std = StdUdpSocket::bind(socket_addr)?;

        let operation = EventQueue::with_current(|queue| -> io::Result<OperationProvider> {
            match queue.kind() {
                #[cfg(feature = "io-uring")]
                EventQueueKind::IoUring(io_uring) => {
                    if let Some(op) = unsafe { super::io_uring::UdpSendRecv::try_new(io_uring, std.as_raw_fd())? } {
                        return Ok(OperationProvider::IoUringSendRecv(op));
                    }
                }
                _ => {}
            }

            // Fall back to polling mode
            std.set_nonblocking(true)?;
            let poller = unsafe { Poller::new(std.as_raw_fd())? };
            Ok(OperationProvider::Poll(poller))
        })?;

        Ok(UdpSocket {
            std: Arc::new(std),
            operation,
        })
    }

    pub async fn send_to<'a, B>(&'a mut self, buffer: B, addr: SocketAddr) -> io::Result<usize>
    where
        B: RawOutputBuffer + 'a,
    {
        match &mut self.operation {
            OperationProvider::Poll(poller) => {
                let mut buffer = buffer.take();
                loop {
                    match B::visit(
                        &mut buffer,
                        SendToVisitor {
                            socket: &self.std,
                            addr,
                        },
                    ) {
                        Ok(value) => return Ok(value),
                        Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                            poller.wait_for_write().await?;
                        }
                        Err(err) => return Err(err),
                    }
                }
            }
            #[cfg(feature = "io-uring")]
            OperationProvider::IoUringSendRecv(op) => return op.send_to(buffer, addr).await,
        }
    }

    pub async fn recv_from<'a, B>(&'a mut self, buffer: B) -> io::Result<(usize, SocketAddr)>
    where
        B: RawInputBuffer + 'a,
    {
        match &mut self.operation {
            OperationProvider::Poll(poller) => {
                let mut buffer = buffer.take();
                loop {
                    match B::visit(&mut buffer, RecvFromVisitor { socket: &self.std }) {
                        Ok((len, addr)) => {
                            B::finalize(buffer, len);
                            return Ok((len, addr));
                        }
                        Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                            poller.wait_for_read().await?;
                        }
                        Err(err) => return Err(err),
                    }
                }
            }
            #[cfg(feature = "io-uring")]
            OperationProvider::IoUringSendRecv(op) => return op.recv_from(buffer).await,
        }
    }

    #[inline]
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.std.local_addr()
    }

    #[inline]
    pub fn clone(&self) -> io::Result<UdpSocket> {
        Ok(UdpSocket {
            std: self.std.clone(),
            operation: match &self.operation {
                OperationProvider::Poll(_) => todo!(),
                OperationProvider::IoUringSendRecv(op) => OperationProvider::IoUringSendRecv(op.clone()),
            },
        })
    }
}

struct SendToVisitor<'a> {
    socket: &'a StdUdpSocket,
    addr: SocketAddr,
}

impl OutputBufferVisitor for SendToVisitor<'_> {
    type Output = io::Result<usize>;

    #[inline]
    fn unpinned_slice(self, data: &[u8]) -> Self::Output {
        self.socket.send_to(data, self.addr)
    }

    #[inline]
    fn unpinned_vector(self, data: &[&[u8]]) -> Self::Output {
        todo!()
    }
}

struct RecvFromVisitor<'a> {
    socket: &'a StdUdpSocket,
}

impl InputBufferVisitor for RecvFromVisitor<'_> {
    type Output = io::Result<(usize, SocketAddr)>;

    #[inline]
    fn unpinned_slice(self, data: &mut [MaybeUninit<u8>]) -> Self::Output {
        unsafe {
            // FIXME: this is dangerous to do with `MaybeUninit`, use a direct call instead!
            self.socket
                .recv_from(std::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, data.len()))
        }
    }

    #[inline]
    fn unpinned_vector(self, data: &[&mut [MaybeUninit<u8>]]) -> Self::Output {
        todo!()
    }
}

impl AsRawFd for crate::UdpSocket {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.imp.std.as_raw_fd()
    }
}
