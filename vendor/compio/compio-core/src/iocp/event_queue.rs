use std::{convert::TryFrom, io, mem, os::windows::prelude::AsRawHandle, ptr, sync::Arc, time::Duration};

use ::tracing::error;
use tracing::trace_span;
use winapi::{
    shared::{
        minwindef::{DWORD, FALSE, ULONG},
        winerror::ERROR_TIMEOUT,
    },
    um::{
        handleapi::INVALID_HANDLE_VALUE,
        ioapiset::{CreateIoCompletionPort, GetQueuedCompletionStatusEx, PostQueuedCompletionStatus},
        minwinbase::{OVERLAPPED, OVERLAPPED_ENTRY},
        winbase::INFINITE,
    },
};
use winhandle::WinHandle;

use compio_internal_util::{winapi_bool_call, winapi_handle_call};

use crate::{
    event_queue::{EventQueueFactory, EventQueueImpl, HandleImpl},
    iocp::operation::OperationOverlapped,
    platform::ext::{EventQueueExt, EventQueueImplExt},
};

pub struct Iocp {
    shared: Arc<Shared>,
    custom_events: Vec<Box<dyn Fn(usize) + Send + Sync>>,
}

struct Shared {
    iocp: WinHandle,
}

impl Drop for Iocp {
    fn drop(&mut self) {
        // FIXME: we should pull off any queued completion results and drop them appropriately to prevent leaks
    }
}

#[derive(Clone, Default, Debug)]
pub struct Options {
    max_concurrent_threads: Option<usize>,
}

const HANDLE_COMPLETION_KEY: usize = 1;
const FIRST_CUSTOM_EVENT_COMPLETION_KEY: usize = 2;

impl Iocp {
    pub fn new() -> io::Result<Iocp> {
        Self::with_options(&Options::default())
    }

    pub fn with_options(options: &Options) -> io::Result<Iocp> {
        let threads = options.max_concurrent_threads.unwrap_or_else(|| num_cpus::get());
        unsafe {
            let threads = DWORD::try_from(threads).expect("thread count does not fit in DWORD");
            let iocp = winapi_handle_call!(null_on_error: CreateIoCompletionPort(INVALID_HANDLE_VALUE, ptr::null_mut(), 0, threads))?;
            let shared = Arc::new(Shared { iocp });
            Ok(Iocp {
                shared,
                custom_events: Vec::new(),
            })
        }
    }

    pub fn associate(&self, handle: &impl AsRawHandle) -> io::Result<()> {
        unsafe {
            let span = trace_span!("CreateIoCompletionPort");
            let _guard = span.enter();
            let ret = CreateIoCompletionPort(
                handle.as_raw_handle() as _,
                self.shared.iocp.get() as _,
                HANDLE_COMPLETION_KEY,
                0,
            );
            if ret.is_null() {
                let err = io::Error::last_os_error();
                error!(
                    what = "win32_error",
                    function = ::std::stringify!($func_name),
                    error_code = ?err.raw_os_error(),
                    "function CreateIoCompletionPort failed: {}", err);
                return Err(err);
            }
            debug_assert_eq!(ret as usize, self.shared.iocp.get() as usize);
            Ok(())
        }
    }

    fn poll_internal(
        &self,
        completion_buffer: &mut [OVERLAPPED_ENTRY],
        timeout: Option<Duration>,
    ) -> io::Result<usize> {
        unsafe {
            let entries = {
                let span = trace_span!("GetQueuedCompletionStatusEx");
                let _guard = span.enter();

                let timeout = match timeout {
                    Some(timeout) => DWORD::try_from(timeout.as_millis()).expect("timeout too large"),
                    None => INFINITE,
                };
                let mut entries_removed: ULONG = 0;
                if GetQueuedCompletionStatusEx(
                    self.shared.iocp.get(),
                    completion_buffer.as_mut_ptr(),
                    // Truncation is fine here because it's an input buffer size
                    completion_buffer.len() as DWORD,
                    &mut entries_removed,
                    timeout,
                    FALSE,
                ) == FALSE
                {
                    let err = io::Error::last_os_error();
                    if err.raw_os_error() == Some(ERROR_TIMEOUT as i32) {
                        return Ok(0);
                    }
                    error!(
                        what = "win32_error",
                        function = ::std::stringify!($func_name),
                        error_code = ?err.raw_os_error(),
                        "function {} failed: {}", ::std::stringify!($func_name), err);
                    return Err(err);
                }
                &completion_buffer[..(entries_removed as usize)]
            };

            for entry in entries {
                match entry.lpCompletionKey as usize {
                    HANDLE_COMPLETION_KEY => {
                        let operation_overlapped =
                            &*(entry.lpOverlapped as *const OVERLAPPED as *const OperationOverlapped);
                        operation_overlapped.waker.wake();
                    }
                    key => {
                        if let Some(custom_event) = key
                            .checked_sub(FIRST_CUSTOM_EVENT_COMPLETION_KEY)
                            .and_then(|key| self.custom_events.get(key))
                        {
                            custom_event(entry.lpOverlapped as usize)
                        } else {
                            error!(completion_key = key, "unexpected completion key");
                            return Err(io::Error::new(io::ErrorKind::Other, "unexpected completion key"));
                        }
                    }
                }
            }

            Ok(entries.len())
        }
    }
}

// TODO: don't pull this out of our asses
const DEFAULT_POLL_COMPLETION_BUFFER_SIZE: usize = 64;

impl EventQueueImpl for Iocp {
    fn poll(&self, timeout: Option<Duration>) -> io::Result<usize> {
        let mut completion_buffer: [OVERLAPPED_ENTRY; DEFAULT_POLL_COMPLETION_BUFFER_SIZE] = unsafe { mem::zeroed() };
        self.poll_internal(&mut completion_buffer, timeout)
    }

    fn poll_mut(&mut self, timeout: Option<Duration>) -> io::Result<usize> {
        self.poll(timeout)
    }

    fn handle(&self) -> std::sync::Arc<dyn HandleImpl> {
        self.shared.clone()
    }

    fn new_custom_event(&mut self, callback: Box<dyn Fn(usize) + Send + Sync>) -> usize {
        self.custom_events.push(callback);
        let key = FIRST_CUSTOM_EVENT_COMPLETION_KEY + (self.custom_events.len() - 1);
        key
    }

    fn wake(&self) -> io::Result<()> {
        todo!()
    }

    fn as_iocp(&self) -> &Iocp {
        self
    }
}

impl EventQueueImplExt for Iocp {}

impl HandleImpl for Shared {
    fn enqueue_custom_event(&self, key: usize, data: usize) -> io::Result<()> {
        unsafe { winapi_bool_call!(PostQueuedCompletionStatus(self.iocp.get(), 0, key as _, data as _)) }
    }

    fn wake(&self) -> io::Result<()> {
        todo!()
    }
}

impl EventQueueExt for crate::EventQueue {
    fn as_iocp(&self) -> &Iocp {
        self.imp.as_iocp()
    }
}

impl EventQueueFactory for Options {
    fn new(&self) -> io::Result<crate::EventQueue> {
        let iocp = Iocp::new()?;
        Ok(crate::EventQueue::with_imp(Box::new(iocp)))
    }
}
