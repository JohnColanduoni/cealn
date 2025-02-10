use std::{
    cmp,
    convert::TryInto,
    io,
    mem::{self, MaybeUninit},
    ptr,
};

use compio_core::{
    buffer::{BufferRequest, InputBufferVisitor, OutputBufferVisitor, RawInputBuffer, RawOutputBuffer},
    kqueue::MachRegistration,
    os::macos::EventQueueExt,
    EventQueue,
};

use libc::vm_deallocate;
use mach::{
    mach_port::{mach_port_allocate, mach_port_extract_right, mach_port_insert_right},
    message::{
        mach_msg, mach_msg_size_t, mach_msg_trailer_t, MACH_MSGH_BITS, MACH_MSGH_BITS_COMPLEX, MACH_MSG_OOL_DESCRIPTOR,
        MACH_MSG_PHYSICAL_COPY, MACH_MSG_TIMEOUT_NONE, MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND,
        MACH_MSG_VIRTUAL_COPY, MACH_RCV_MSG, MACH_RCV_TIMED_OUT, MACH_RCV_TIMEOUT, MACH_SEND_MSG, MACH_SEND_TIMED_OUT,
        MACH_SEND_TIMEOUT,
    },
    port::{mach_port_name_t, MACH_PORT_NULL, MACH_PORT_RIGHT_RECEIVE},
    traps::mach_task_self,
    vm::mach_vm_deallocate,
    vm_types::{mach_vm_address_t, mach_vm_size_t},
};
use tracing::trace_span;

use crate::{message_channel::ReceiveResult, platform::mach::kern_return_err};

pub use compio_core::buffer::AllocBuffer as IdealInputBuffer;

pub use compio_core::buffer::AllocBuffer as IdealOutputBuffer;

pub struct MessageChannel {
    send: Port,
    recv: Port,
    recv_registration: MachRegistration,
}

struct Port(mach::port::mach_port_name_t);

impl Drop for Port {
    fn drop(&mut self) {
        unsafe {
            mach::mach_port::mach_port_deallocate(mach::traps::mach_task_self(), self.0);
        }
    }
}

pub trait MessageChannelExt: Sized {
    unsafe fn from_rights(send_right: mach_port_name_t, recv_right: mach_port_name_t) -> io::Result<Self>;

    fn recv_right(&self) -> mach_port_name_t;
    fn send_right(&self) -> mach_port_name_t;
}

impl MessageChannel {
    pub fn pair() -> io::Result<(Self, Self)> {
        unsafe {
            let a_recv = recv_only_port()?;
            let b_recv = recv_only_port()?;
            // Connect ports by giving each a send right to the other
            let b_send = copy_send_right(a_recv.0)?;
            let a_send = copy_send_right(b_recv.0)?;
            let a = MessageChannel::from_ports(a_send, a_recv)?;
            let b = MessageChannel::from_ports(b_send, b_recv)?;
            Ok((a, b))
        }
    }

    unsafe fn from_ports(send: Port, recv: Port) -> io::Result<Self> {
        let recv_registration =
            EventQueue::with_current(|event_queue| MachRegistration::register(event_queue.kqueue(), recv.0))?;
        Ok(MessageChannel {
            send,
            recv,
            recv_registration,
        })
    }

    pub async fn send<B>(&self, buffer: B) -> io::Result<()>
    where
        B: RawOutputBuffer,
    {
        let mut buffer = buffer.take();
        loop {
            match B::visit(&mut buffer, SendVisitor { port: self.send.0 }) {
                Ok(()) => return Ok(()),
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                    todo!()
                }
                Err(err) => return Err(err),
            }
        }
    }

    pub async fn recv<B>(&self, buffer: B) -> io::Result<ReceiveResult>
    where
        B: RawInputBuffer,
    {
        let mut buffer = buffer.take();
        loop {
            match B::visit(&mut buffer, RecvVisitor { port: self.recv.0 }) {
                Ok(res @ ReceiveResult::Full(len)) => {
                    B::finalize(buffer, len);
                    return Ok(res);
                }
                Ok(res @ ReceiveResult::Partial { bytes_read, .. }) => {
                    B::finalize(buffer, bytes_read);
                    return Ok(res);
                }
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                    self.recv_registration.wait_for_read().await?;
                }
                Err(err) => return Err(err),
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
    #[inline]
    unsafe fn from_rights(send_right: mach_port_name_t, recv_right: mach_port_name_t) -> io::Result<Self> {
        let imp = MessageChannel::from_ports(Port(send_right), Port(recv_right))?;
        Ok(crate::MessageChannel { imp })
    }

    #[inline]
    fn recv_right(&self) -> mach_port_name_t {
        self.imp.recv.0
    }

    #[inline]
    fn send_right(&self) -> mach_port_name_t {
        self.imp.send.0
    }
}

fn recv_only_port() -> io::Result<Port> {
    unsafe {
        let mut port: mach_port_name_t = MACH_PORT_NULL as mach_port_name_t;
        mach_call!(mach_port_allocate(mach_task_self(), MACH_PORT_RIGHT_RECEIVE, &mut port))?;
        Ok(Port(port))
    }
}

unsafe fn copy_send_right(receiver: mach_port_name_t) -> io::Result<Port> {
    mach_call!(mach_port_insert_right(
        mach_task_self(),
        receiver,
        receiver,
        MACH_MSG_TYPE_MAKE_SEND,
    ))?;
    let mut port: mach_port_name_t = MACH_PORT_NULL as mach_port_name_t;
    let mut received_ty = 0;
    mach_call!(mach_port_extract_right(
        mach_task_self(),
        receiver,
        MACH_MSG_TYPE_COPY_SEND,
        &mut port,
        &mut received_ty,
    ))?;
    let port = Port(port);
    Ok(port)
}

const MESSAGE_ID_SMALL: i32 = 3;

// FIXME: this is high because capbox doesn't send small messages correctly, fix that and lower it
const MAX_SMALL_MESSAGE_BUFFER_SIZE: usize = 80 * 1024;

#[derive(Debug)]
#[repr(C)]
struct SmallMessage {
    header: mach::message::mach_msg_header_t,
    body: mach::message::mach_msg_body_t,
    packet_len: usize,
    // FIXME: make the sent message variable size to reduce unecessary copying. It will also allow us to avoid
    // initializing the full buffer on the send side.
    contents: [u8; MAX_SMALL_MESSAGE_BUFFER_SIZE],
}

#[derive(Debug)]
#[repr(C)]
struct SmallMessageRecv {
    msg: SmallMessage,
    trailer: mach_msg_trailer_t,
}

struct SendVisitor {
    port: mach::port::mach_port_name_t,
}

impl OutputBufferVisitor for SendVisitor {
    type Output = io::Result<()>;

    #[inline]
    fn unpinned_slice(self, data: &[u8]) -> Self::Output {
        // FIXME: accept larger buffers? Make this more consistent between platforms?
        if data.len() > MAX_SMALL_MESSAGE_BUFFER_SIZE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "buffer too large for MessageChannel",
            ))?;
        }

        unsafe {
            let mut message = SmallMessage {
                header: mach::message::mach_msg_header_t {
                    // FIXME: verify if we need to clean the remote port up on the receiver's side
                    msgh_bits: MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0),
                    msgh_size: mem::size_of::<SmallMessage>() as u32,
                    msgh_remote_port: self.port,
                    msgh_local_port: 0,
                    msgh_voucher_port: 0,
                    msgh_id: MESSAGE_ID_SMALL,
                },
                body: mach::message::mach_msg_body_t {
                    msgh_descriptor_count: 0,
                },
                packet_len: data.len(),
                contents: [0u8; MAX_SMALL_MESSAGE_BUFFER_SIZE],
            };
            message.contents[..data.len()].copy_from_slice(data);

            loop {
                let span = trace_span!("mach_msg");
                let _guard = span.enter();

                let ret = mach_msg(
                    &mut message.header,
                    MACH_SEND_MSG | MACH_SEND_TIMEOUT,
                    mem::size_of::<SmallMessage>() as mach_msg_size_t,
                    0,
                    MACH_PORT_NULL as mach_port_name_t,
                    MACH_MSG_TIMEOUT_NONE,
                    MACH_PORT_NULL as mach_port_name_t,
                );
                if ret == 0 {
                    return Ok(());
                } else if ret == MACH_SEND_TIMED_OUT {
                    return Err(io::ErrorKind::WouldBlock.into());
                } else {
                    // FIXME: handle interrupts
                    let err = kern_return_err(ret);
                    return Err(err);
                }
            }
        }
    }

    #[inline]
    fn unpinned_vector(self, data: &[&[u8]]) -> Self::Output {
        todo!()
    }
}

struct RecvVisitor {
    port: mach::port::mach_port_name_t,
}

impl InputBufferVisitor for RecvVisitor {
    type Output = io::Result<ReceiveResult>;

    #[inline]
    fn unpinned_slice(self, data: &mut [MaybeUninit<u8>]) -> Self::Output {
        unsafe {
            loop {
                let span = trace_span!("mach_msg");
                let _guard = span.enter();

                // FIXME: don't initialize this, it's pretty large
                let mut message: SmallMessageRecv = mem::zeroed();

                // FIXME: make sure this is non-blocking, not sure if that's what TIMEOUT_NONE means
                let ret = mach_msg(
                    &mut message.msg.header,
                    MACH_RCV_MSG | MACH_RCV_TIMEOUT,
                    0,
                    mem::size_of::<SmallMessageRecv>() as mach_msg_size_t,
                    self.port,
                    0,
                    MACH_PORT_NULL as mach_port_name_t,
                );
                if ret == 0 {
                    let recv_size = cmp::min(message.msg.packet_len, data.len());
                    ptr::copy(message.msg.contents.as_ptr(), data.as_mut_ptr() as *mut u8, recv_size);
                    if recv_size == message.msg.packet_len {
                        return Ok(ReceiveResult::Full(recv_size));
                    } else {
                        return Ok(ReceiveResult::Partial {
                            bytes_read: recv_size,
                            total_size: message.msg.packet_len,
                        });
                    }
                } else if ret == MACH_RCV_TIMED_OUT {
                    return Err(io::ErrorKind::WouldBlock.into());
                } else {
                    // FIXME: interrupts
                    let err = kern_return_err(ret);
                    return Err(err);
                }
            }
        }
    }

    #[inline]
    fn unpinned_vector(self, data: &[&mut [MaybeUninit<u8>]]) -> Self::Output {
        todo!()
    }
}
