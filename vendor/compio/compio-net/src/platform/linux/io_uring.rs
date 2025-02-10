use std::{
    cell::UnsafeCell,
    io,
    mem::{self, ManuallyDrop, MaybeUninit},
    net::SocketAddr,
    os::unix::prelude::RawFd,
    pin::Pin,
    ptr,
};

use compio_core::{
    buffer::{self, RawInputBuffer, RawOutputBuffer},
    io_uring::{CompletionWakerStorage, CurrentEventQueueSubmitterSource, IoUring},
};

use io_uring::opcode::{RecvMsg, SendMsg};
use os_socketaddr::OsSocketAddr;
use pin_project::pin_project;
use smallvec::SmallVec;

pub(crate) struct UdpSendRecv {
    fd: RawFd,
    // This must be kept alive as long as there is a pending operation, so we prevent it from being automatically dropped
    data: ManuallyDrop<Pin<Box<UdpSendRecvData>>>,
}

impl Drop for UdpSendRecv {
    fn drop(&mut self) {
        unsafe {
            // Delay release of the allocation holding the callback if there is a pending operation
            let callback_ptr = self.data.as_mut().project().callback.get_unchecked_mut() as *mut _;
            let allocation = ManuallyDrop::take(&mut self.data);
            CompletionWakerStorage::ensure_cleanup(callback_ptr, move || mem::drop(allocation));
        }
    }
}

#[pin_project]
struct UdpSendRecvData {
    #[pin]
    callback: CompletionWakerStorage,
    // Unique access to `MsghdrStorage` is managed by unique access to `UdpSendRecv`, but we place it in the completion
    // callback to pin it and ensure it lasts for the duration of the call
    msghdr: UnsafeCell<MsghdrStorage>,
}

unsafe impl Send for UdpSendRecvData {}
unsafe impl Sync for UdpSendRecvData {}

struct MsghdrStorage {
    msghdr: libc::msghdr,
    sockaddr: OsSocketAddr,
    iov: SmallVec<[libc::iovec; NON_ALLOC_IOVEC_LEN]>,
}

unsafe impl Send for MsghdrStorage {}
unsafe impl Sync for MsghdrStorage {}

const NON_ALLOC_IOVEC_LEN: usize = 16;

impl UdpSendRecv {
    pub unsafe fn try_new(io_uring: &IoUring, fd: RawFd) -> io::Result<Option<UdpSendRecv>> {
        // We need sendmsg and recvmsg support to use UDP sockets with io_uring in this manner
        if !(io_uring.probe().is_supported(SendMsg::CODE) && io_uring.probe().is_supported(RecvMsg::CODE)) {
            return Ok(None);
        }
        let data = UdpSendRecv {
            fd,
            data: ManuallyDrop::new(Box::pin(UdpSendRecvData {
                callback: CompletionWakerStorage::new(),
                msghdr: UnsafeCell::new(MsghdrStorage {
                    msghdr: mem::zeroed(),
                    sockaddr: mem::zeroed(),
                    iov: SmallVec::new(),
                }),
            })),
        };
        Ok(Some(data))
    }

    #[inline]
    pub fn clone(&self) -> Self {
        let data = UdpSendRecv {
            fd: self.fd,
            data: ManuallyDrop::new(Box::pin(UdpSendRecvData {
                callback: CompletionWakerStorage::new(),
                msghdr: UnsafeCell::new(MsghdrStorage {
                    msghdr: unsafe { mem::zeroed() },
                    sockaddr: unsafe { mem::zeroed() },
                    iov: SmallVec::new(),
                }),
            })),
        };
        data
    }

    pub async fn send_to<'a, B>(&'a mut self, buffer: B, addr: SocketAddr) -> io::Result<usize>
    where
        B: RawOutputBuffer + 'a,
    {
        unsafe {
            // FIXME: allow pinned vectorized buffers here without copies
            let buffer = buffer::ensure_pinned_output(buffer);
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            // Don't dereference msghdr until we know we have unique access to it; an ongoing operation may be in
            // progress until the prepare callback of `submit` below.
            let msghdr = data.msghdr.get() as usize;
            let fd = self.fd;

            let result = callback
                .submit(&mut submitter, {
                    let buffer = &buffer;
                    move || {
                        // We know the msghdr is no longer in use
                        let msghdr = &mut *(msghdr as *mut MsghdrStorage);
                        msghdr.write_for_send(Some(addr), std::iter::once(&**buffer), libc::MSG_NOSIGNAL);
                        SendMsg::new(io_uring::types::Fd(fd), msghdr.raw()).build()
                    }
                })
                .await;
            // TODO: handle releasing the temporary buffer when canceling
            buffer.release();
            let bytes_written = result?;
            Ok(bytes_written as usize)
        }
    }

    #[inline]
    pub async fn recv_from<'a, B>(&'a mut self, buffer: B) -> io::Result<(usize, SocketAddr)>
    where
        B: RawInputBuffer + 'a,
    {
        unsafe {
            // FIXME: allow pinned vectorized buffers here without copies
            let mut buffer = buffer::ensure_pinned_input(buffer);
            let mut submitter = CurrentEventQueueSubmitterSource;
            let data = self.data.as_mut().project();
            let callback = data.callback;
            // Don't dereference msghdr until we know we have unique access to it; an ongoing operation may be in
            // progress until the prepare callback of `submit` below.
            let msghdr = data.msghdr.get() as usize;
            let fd = self.fd;

            match callback
                .submit(&mut submitter, {
                    let buffer = &mut buffer;
                    move || {
                        // We know the msghdr is no longer in use
                        let msghdr = &mut *(msghdr as *mut MsghdrStorage);
                        msghdr.write_for_recv(std::iter::once(&mut **buffer), 0);
                        RecvMsg::new(io_uring::types::Fd(fd), msghdr.raw()).build()
                    }
                })
                .await
            {
                Ok(bytes_read) => {
                    buffer.finalize(bytes_read as usize);
                    let msghdr = &mut *(msghdr as *mut MsghdrStorage);
                    Ok((bytes_read as usize, msghdr.read_sockaddr()))
                }
                Err(err) => {
                    buffer.release();
                    Err(err)
                }
            }
        }
    }
}

impl MsghdrStorage {
    fn write_for_send<'a>(
        &mut self,
        socket_addr: Option<SocketAddr>,
        buffers: impl Iterator<Item = &'a [u8]>,
        flags: i32,
    ) {
        let msg_name;
        let msg_namelen;
        match socket_addr {
            Some(socket_addr) => {
                self.sockaddr = OsSocketAddr::from(socket_addr);
                msg_name = self.sockaddr.as_ptr() as _;
                msg_namelen = self.sockaddr.len();
            }
            None => {
                msg_name = ptr::null_mut();
                msg_namelen = 0;
            }
        }

        self.iov.clear();
        self.iov.extend(buffers.map(|item| libc::iovec {
            iov_base: item.as_ptr() as _,
            iov_len: item.len(),
        }));

        self.msghdr = libc::msghdr {
            msg_name,
            msg_namelen: msg_namelen as _,
            msg_iov: self.iov.as_ptr() as _,
            msg_iovlen: self.iov.len(),
            msg_control: ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: flags,
        };
    }

    fn write_for_recv<'a>(&mut self, buffers: impl Iterator<Item = &'a mut [MaybeUninit<u8>]>, flags: i32) {
        let msg_name = self.sockaddr.as_ptr() as _;
        let msg_namelen = self.sockaddr.capacity();
        self.sockaddr = OsSocketAddr::new();

        self.iov.clear();
        self.iov.extend(buffers.map(|item| libc::iovec {
            iov_base: item.as_ptr() as _,
            iov_len: item.len(),
        }));

        self.msghdr = libc::msghdr {
            msg_name,
            msg_namelen: msg_namelen as _,
            msg_iov: self.iov.as_ptr() as _,
            msg_iovlen: self.iov.len(),
            msg_control: ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: flags,
        };
    }

    fn read_sockaddr(&self) -> SocketAddr {
        self.sockaddr.into_addr().unwrap()
    }

    #[inline]
    fn raw(&mut self) -> *mut libc::msghdr {
        &mut self.msghdr
    }
}
