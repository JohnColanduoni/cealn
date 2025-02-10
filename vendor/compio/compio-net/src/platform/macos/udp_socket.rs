use std::{
    io,
    mem::MaybeUninit,
    net::{SocketAddr, UdpSocket as StdUdpSocket},
    os::unix::prelude::{AsRawFd, RawFd},
};

use compio_core::{
    buffer::{InputBufferVisitor, OutputBufferVisitor, RawInputBuffer, RawOutputBuffer},
    kqueue::Registration,
    os::macos::*,
    EventQueue,
};

pub(crate) struct UdpSocket {
    std: StdUdpSocket,
    registration: Registration,
}

impl UdpSocket {
    pub(crate) fn bind(socket_addr: SocketAddr) -> io::Result<UdpSocket> {
        let std = StdUdpSocket::bind(socket_addr)?;

        let registration = EventQueue::with_current(|queue| -> io::Result<Registration> {
            let kqueue = queue.kqueue();
            // Fall back to polling mode
            std.set_nonblocking(true)?;
            let registration = unsafe { Registration::register(kqueue, std.as_raw_fd())? };
            Ok(registration)
        })?;

        Ok(UdpSocket { std, registration })
    }

    pub async fn send_to<'a, B>(&'a self, buffer: B, addr: SocketAddr) -> io::Result<usize>
    where
        B: RawOutputBuffer + 'a,
    {
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
                    self.registration.wait_for_write().await?;
                }
                Err(err) => return Err(err),
            }
        }
    }

    pub async fn recv_from<'a, B>(&'a self, buffer: B) -> io::Result<(usize, SocketAddr)>
    where
        B: RawInputBuffer + 'a,
    {
        let mut buffer = buffer.take();
        loop {
            match B::visit(&mut buffer, RecvFromVisitor { socket: &self.std }) {
                Ok((len, addr)) => {
                    B::finalize(buffer, len);
                    return Ok((len, addr));
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                    self.registration.wait_for_read().await?;
                }
                Err(err) => return Err(err),
            }
        }
    }

    #[inline]
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.std.local_addr()
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
