use std::{
    io,
    net::{SocketAddr, ToSocketAddrs},
};

use futures::prelude::*;

use compio_core::buffer::{RawInputBuffer, RawOutputBuffer};

use crate::platform;

pub struct UdpSocket {
    pub(crate) imp: platform::udp_socket::UdpSocket,
}

impl UdpSocket {
    pub fn bind(addr: impl ToSocketAddrs) -> io::Result<UdpSocket> {
        let socket_addr = addr.to_socket_addrs()?.next().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::AddrNotAvailable,
                "provided address resolved to no address",
            )
        })?;
        let imp = platform::udp_socket::UdpSocket::bind(socket_addr)?;
        Ok(UdpSocket { imp })
    }

    #[tracing::instrument(level = "trace", err, skip(self, buffer))]
    #[inline]
    pub async fn send_to<'a>(&'a mut self, buffer: impl RawOutputBuffer + 'a, addr: SocketAddr) -> io::Result<usize> {
        self.imp.send_to(buffer, addr).await
    }

    #[tracing::instrument(level = "trace", err, skip(self, buffer))]
    #[inline]
    pub async fn recv_from<'a>(&'a mut self, buffer: impl RawInputBuffer + 'a) -> io::Result<(usize, SocketAddr)> {
        self.imp.recv_from(buffer).await
    }

    #[inline]
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.imp.local_addr()
    }

    #[inline]
    pub fn clone(&self) -> io::Result<UdpSocket> {
        let imp = self.imp.clone()?;
        Ok(UdpSocket { imp })
    }
}

#[cfg(test)]
mod tests {
    use std::{mem, task::Poll};

    use compio_core::buffer::AllowTake;
    use compio_executor::LocalPool;
    use futures::pin_mut;

    use super::*;

    #[test]
    fn test_udp_send_recv() {
        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let left = UdpSocket::bind("127.0.0.1:0").unwrap();
            let right = UdpSocket::bind("127.0.0.1:0").unwrap();
            let right_addr = right.local_addr().unwrap();

            let mut buffer = Vec::with_capacity(128);
            let (byte_count, addr) = {
                let do_recv = right.recv_from(AllowTake(&mut buffer));
                pin_mut!(do_recv);
                match futures::poll!(&mut do_recv) {
                    Poll::Ready(Ok(_)) => panic!("expected not ready"),
                    Poll::Ready(Err(err)) => panic!("error: {:?}", err),
                    Poll::Pending => {}
                };

                left.send_to(&b"abcdefg"[..], right_addr).await.unwrap();

                do_recv.await.unwrap()
            };
            assert_eq!(addr, left.local_addr().unwrap());
            assert_eq!(byte_count, buffer.len());
            assert_eq!(&*buffer, b"abcdefg");
        });
    }

    #[test]
    fn test_udp_three_way() {
        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let left = UdpSocket::bind("127.0.0.1:0").unwrap();
            let right = UdpSocket::bind("127.0.0.1:0").unwrap();
            let left_addr = left.local_addr().unwrap();
            let right_addr = right.local_addr().unwrap();

            let mut buffer = Vec::with_capacity(128);
            let (byte_count, addr) = {
                let do_recv = right.recv_from(AllowTake(&mut buffer));
                pin_mut!(do_recv);
                match futures::poll!(&mut do_recv) {
                    Poll::Ready(Ok(_)) => panic!("expected not ready"),
                    Poll::Ready(Err(err)) => panic!("error: {:?}", err),
                    Poll::Pending => {}
                };

                left.send_to(&b"abcdefg"[..], right_addr).await.unwrap();

                do_recv.await.unwrap()
            };
            assert_eq!(addr, left.local_addr().unwrap());
            assert_eq!(byte_count, buffer.len());
            assert_eq!(&*buffer, b"abcdefg");

            let mut buffer = Vec::with_capacity(128);
            let (byte_count, addr) = {
                let do_recv = left.recv_from(AllowTake(&mut buffer));
                pin_mut!(do_recv);
                match futures::poll!(&mut do_recv) {
                    Poll::Ready(Ok(_)) => panic!("expected not ready"),
                    Poll::Ready(Err(err)) => panic!("error: {:?}", err),
                    Poll::Pending => {}
                };

                right.send_to(&b"gfedcba"[..], left_addr).await.unwrap();

                do_recv.await.unwrap()
            };
            assert_eq!(addr, right_addr);
            assert_eq!(byte_count, buffer.len());
            assert_eq!(&*buffer, b"gfedcba");
        });
    }

    #[test]
    fn test_udp_send_buffer_fill() {
        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let left = UdpSocket::bind("0.0.0.0:0").unwrap();
            // Reserved IP address range
            let right_addr = SocketAddr::from(([240, 0, 0, 1], 1000));

            let message: &'static [u8] = &[0u8; 1024];
            for _ in 0..1000 {
                let do_send = left.send_to(message, right_addr);
                pin_mut!(do_send);
                match futures::poll!(&mut do_send) {
                    Poll::Ready(Ok(_)) => continue,
                    Poll::Ready(Err(err)) => panic!("error: {:?}", err),
                    Poll::Pending => {
                        println!("pending hit");
                        // Ensure that we can finish the future if needed
                        do_send.await.unwrap();
                        return;
                    }
                };
            }
            panic!("failed to fill send buffer");
        });
    }
}
