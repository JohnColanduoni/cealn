use std::{convert::TryFrom, ffi::OsString, io, mem, os::windows::io::RawHandle, ptr};

use tracing::{debug_span, error};
use uuid::Uuid;
use widestring::WideCString;
use winapi::{
    ctypes::c_void,
    shared::{
        minwindef::{DWORD, FALSE, TRUE},
        winerror::{ERROR_BROKEN_PIPE, ERROR_IO_PENDING, ERROR_MORE_DATA},
    },
    um::{
        fileapi::{CreateFileW, ReadFile, WriteFile, OPEN_EXISTING},
        ioapiset::GetOverlappedResultEx,
        minwinbase::{OVERLAPPED, SECURITY_ATTRIBUTES},
        namedpipeapi::{ConnectNamedPipe, CreateNamedPipeW},
        securitybaseapi::{InitializeSecurityDescriptor, SetSecurityDescriptorDacl},
        winbase::{
            SetFileCompletionNotificationModes, FILE_FLAG_OVERLAPPED, FILE_SKIP_COMPLETION_PORT_ON_SUCCESS,
            PIPE_ACCESS_DUPLEX, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_MESSAGE,
            SECURITY_IDENTIFICATION,
        },
        winnt::{GENERIC_READ, GENERIC_WRITE, SECURITY_DESCRIPTOR},
    },
};
use winhandle::WinHandle;

use compio_core::{
    buffer::{self, BufferRequest, RawInputBuffer, RawOutputBuffer},
    iocp::Operation,
    os::windows::EventQueueExt,
    EventQueue,
};
use compio_internal_util::{winapi_bool_call, winapi_handle_call};

use crate::message_channel::ReceiveResult;

use super::ext::MessageChannelExt;

pub struct MessageChannel {
    named_pipe: WinHandle,
}

pub use compio_core::iocp::OperationAllocBuffer as IdealOutputBuffer;

pub use compio_core::iocp::OperationAllocBuffer as IdealInputBuffer;

impl MessageChannel {
    fn from_handle_with_queue(evqueue: &EventQueue, handle: WinHandle) -> io::Result<Self> {
        unsafe {
            let iocp = evqueue.as_iocp();
            // Disable completion notification on fast-path
            winapi_bool_call!(SetFileCompletionNotificationModes(
                handle.get() as _,
                FILE_SKIP_COMPLETION_PORT_ON_SUCCESS
            ))?;
            iocp.associate(&handle)?;
        }

        Ok(MessageChannel { named_pipe: handle })
    }

    pub fn pair() -> io::Result<(Self, Self)> {
        let (a, b) = raw_pipe_pair(PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_REJECT_REMOTE_CLIENTS)?;
        EventQueue::with_current(|evqueue| {
            let a = MessageChannel::from_handle_with_queue(evqueue, a)?;
            let b = MessageChannel::from_handle_with_queue(evqueue, b)?;
            Ok((a, b))
        })
    }

    pub async fn send<B>(&self, buffer: B) -> io::Result<()>
    where
        B: RawOutputBuffer,
    {
        unsafe {
            let buffer = buffer::ensure_pinned_output(buffer);
            // Make sure we panic out here to avoid the leaks that can occur if we panic while starting an operation
            let bytes_to_write = DWORD::try_from(buffer.len()).expect("buffer is too large");
            // TODO: get operation slot from buffer to here (if available)
            let (buffer, result) = Operation::start(None, &self.named_pipe, buffer, |handle, overlapped, buffer| {
                let mut number_of_bytes_written: DWORD = 0;
                winapi_bool_call!(io_pending_ok: WriteFile(
                    handle.get(),
                    buffer.as_ptr() as _,
                    bytes_to_write,
                    &mut number_of_bytes_written,
                    overlapped
                ))?;
                Ok(number_of_bytes_written as usize)
            })
            .await;
            buffer.release();
            result?;
            Ok(())
        }
    }

    pub async fn recv<B>(&self, buffer: B) -> io::Result<ReceiveResult>
    where
        B: RawInputBuffer,
    {
        unsafe {
            let buffer = buffer::ensure_pinned_input(buffer);
            // Make sure we panic out here to avoid the leaks that can occur if we panic while starting an operation
            let bytes_to_read = DWORD::try_from(buffer.len()).expect("buffer is too large");
            // TODO: get operation slot from buffer to here (if available)
            let (buffer, result) = Operation::start(None, &self.named_pipe, buffer, |handle, overlapped, buffer| {
                let mut number_of_bytes_read: DWORD = 0;
                winapi_bool_call!(io_pending_ok: ReadFile(
                    handle.get(),
                    buffer.as_ptr() as _,
                    bytes_to_read,
                    &mut number_of_bytes_read,
                    overlapped
                ))?;
                Ok(number_of_bytes_read as usize)
            })
            .await;
            match result {
                Ok(bytes_read) => {
                    buffer.finalize(bytes_read);
                    Ok(ReceiveResult::Full(bytes_read))
                }
                Err(ref err) if err.raw_os_error() == Some(ERROR_MORE_DATA as i32) => {
                    // For consistent behavior accross platforms, we emulate Linux's SOCK_DGRAM/SOCK_SEQPACKET behavior
                    // and discard the data that didn't fit in the buffer, notifying the user of the true length.
                    todo!();
                }
                Err(err) => {
                    buffer.release();
                    Err(err)
                }
            }
        }
    }

    #[inline]
    pub fn new_output_buffer_with_traits(&self, request: BufferRequest) -> io::Result<IdealOutputBuffer> {
        Ok(IdealOutputBuffer::with_traits(request))
    }

    #[inline]
    pub fn new_input_buffer_with_traits(&self, request: BufferRequest) -> io::Result<IdealOutputBuffer> {
        Ok(IdealInputBuffer::with_traits(request))
    }
}

impl MessageChannelExt for crate::MessageChannel {
    unsafe fn from_handle(handle: RawHandle) -> io::Result<Self> {
        let handle = WinHandle::from_raw_unchecked(handle as _);
        let imp = EventQueue::with_current(|evqueue| MessageChannel::from_handle_with_queue(evqueue, handle))?;
        Ok(crate::MessageChannel { imp })
    }
}

fn raw_pipe_pair(pipe_type: DWORD) -> io::Result<(WinHandle, WinHandle)> {
    let span = debug_span!("raw_pipe_pair", pipe_name = tracing::field::Empty);
    let _guard = span.enter();

    unsafe {
        let pipe_name = format!(r#"\\.\pipe\{}"#, Uuid::new_v4());
        span.record("pipe_name", &&*pipe_name);
        let pipe_name = OsString::from(pipe_name);

        // Give pipe a null security descriptor, in case we are running in a sandboxed process
        let mut security_descriptor: SECURITY_DESCRIPTOR = mem::zeroed();
        let lp_security_descriptor: *mut c_void = &mut security_descriptor as *mut SECURITY_DESCRIPTOR as _;
        winapi_bool_call!(InitializeSecurityDescriptor(lp_security_descriptor, 1))?;
        winapi_bool_call!(SetSecurityDescriptorDacl(
            lp_security_descriptor,
            TRUE,
            ptr::null_mut(),
            FALSE,
        ))?;

        let mut security_attributes = SECURITY_ATTRIBUTES {
            nLength: mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD,
            lpSecurityDescriptor: lp_security_descriptor,
            bInheritHandle: false.into(),
        };

        let server_pipe = winapi_handle_call! { CreateNamedPipeW(
            WideCString::from_os_str(&pipe_name).unwrap().as_ptr(),
            PIPE_ACCESS_DUPLEX | FILE_FLAG_OVERLAPPED,
            pipe_type,
            1,
            0, 0,
            0,
            &mut security_attributes,
        )}?;

        // Begin connection operation on server half
        let mut overlapped: OVERLAPPED = mem::zeroed();

        // This should "fail" with ERROR_IO_PENDING since we're doing an overlapped operation on a socket with no
        match winapi_bool_call!(no_error: ConnectNamedPipe(server_pipe.get(), &mut overlapped)) {
            Ok(()) => {
                error!("connection of named pipe should not succeed because we've not started connecting");
                return Err(io::Error::new(
                    io::ErrorKind::Other,
                    "connection of named pipe should not succeed because we've not started connecting",
                ));
            }
            Err(ref err) if err.raw_os_error() == Some(ERROR_IO_PENDING as i32) => {}
            Err(err) => return Err(err),
        }

        let client_pipe = winapi_handle_call!(CreateFileW(
            WideCString::from_os_str(&pipe_name).unwrap().as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            &mut security_attributes,
            OPEN_EXISTING,
            SECURITY_IDENTIFICATION | FILE_FLAG_OVERLAPPED,
            ptr::null_mut()
        ))?;

        let mut bytes: DWORD = 0;
        winapi_bool_call!(GetOverlappedResultEx(
            server_pipe.get(),
            &mut overlapped,
            &mut bytes,
            1000,
            TRUE
        ))?;

        Ok((server_pipe, client_pipe))
    }
}
