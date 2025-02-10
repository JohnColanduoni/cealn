use std::{
    collections::HashMap,
    io,
    pin::Pin,
    sync::Arc,
    time::{Duration, SystemTime},
};

use compio_internal_util::{libc_call, libc_fd_call, unix::ScopedFd};
use io_uring::{
    opcode,
    types::{SubmitArgs, Timespec},
    IoUring as IoUringRaw, Probe,
};
use libc::c_int;
use tracing::debug;

use crate::{
    event_queue::{EventQueueFactory, EventQueueImpl, HandleImpl},
    io_uring::{
        completion::{CustomEventCompleter, WakeCompleter},
        submission::SubmissionFullWakeState,
        CompletionCallback, Options,
    },
    os::linux::{EventQueueImplExt, EventQueueKind, EventQueueKindMut},
};

pub struct IoUring {
    pub(crate) ring: IoUringRaw,
    pub(super) shared: Arc<Shared>,
    pub(super) _wake_completer: Pin<Box<WakeCompleter>>,
}

// TODO: update io_uring::Probe so it's Send and Sync
unsafe impl Send for IoUring {}
unsafe impl Sync for IoUring {}

pub(super) struct Shared {
    probe: Probe,
    events: parking_lot::RwLock<HashMap<usize, RegisteredEvent>>,
    pub(super) submission_full_wake_state: Arc<SubmissionFullWakeState>,
    wake_eventfd: ScopedFd,
}

// TODO: update io_uring::Probe so it's Send and Sync
unsafe impl Send for Shared {}
unsafe impl Sync for Shared {}

struct Handle {
    shared: Arc<Shared>,
}

struct RegisteredEvent {
    pipe_write: ScopedFd,
    _completer: Pin<Box<CustomEventCompleter>>,
}

impl IoUring {
    pub fn new(options: Options) -> io::Result<IoUring> {
        let mut ring = io_uring::IoUring::builder()
            .setup_cqsize(options.cq_size.unwrap_or_else(|| options.entries * 2))
            .setup_clamp()
            .dontfork()
            .build(options.entries)?;

        debug!(params = ?ring.params(), "io_uring created");

        // TODO: Probe support requires 5.6 kernel, come up with some fallback (e.g. feature testing)?
        let mut probe = Probe::new();
        ring.submitter().register_probe(&mut probe)?;

        debug!(?probe, "io_uring support probed");

        let wake_eventfd;
        let wake_completer;
        unsafe {
            wake_eventfd = libc_fd_call!(libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK))?;

            wake_completer = Box::pin(WakeCompleter::new(wake_eventfd.as_raw()));
            let completer_ptr = &*wake_completer as *const WakeCompleter;
            ring.submission()
                .push(
                    &opcode::PollAdd::new(io_uring::types::Fd(wake_eventfd.as_raw()), libc::POLLIN as u32)
                        .multi(true)
                        .build()
                        .user_data(completer_ptr as usize as u64),
                )
                .unwrap();
        };

        let shared = Arc::new(Shared {
            probe,
            events: Default::default(),
            submission_full_wake_state: Arc::new(SubmissionFullWakeState::new()),
            wake_eventfd,
        });
        Ok(IoUring {
            ring,
            shared,
            _wake_completer: wake_completer,
        })
    }

    #[inline]
    pub fn probe(&self) -> &io_uring::Probe {
        &self.shared.probe
    }
}

impl EventQueueFactory for Options {
    fn new(&self) -> io::Result<crate::EventQueue> {
        let ring = IoUring::new(self.clone())?;
        Ok(crate::EventQueue::with_imp(Box::new(ring)))
    }

    fn supports_completion_stealing(&self) -> bool {
        true
    }
}

impl EventQueueImpl for IoUring {
    fn poll(&self, timeout: Option<Duration>) -> io::Result<usize> {
        todo!()
    }

    fn poll_mut(&mut self, timeout: Option<Duration>) -> io::Result<usize> {
        match timeout {
            Some(Duration::ZERO) => loop {
                match self.ring.submitter().submit_and_wait(0) {
                    Ok(_) => break,
                    Err(ref err) if err.raw_os_error() == Some(libc::EINTR) => continue,
                    Err(err) => {
                        return Err(err);
                    }
                }
            },
            Some(timeout) => {
                let timeout_time = (SystemTime::now() + timeout)
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap();
                loop {
                    match self.ring.submitter().submit_with_args(
                        1,
                        &SubmitArgs::new()
                            .timespec(&Timespec::new().sec(timeout_time.as_secs()).nsec(timeout.subsec_nanos())),
                    ) {
                        Ok(_) => break,
                        Err(ref err) if err.raw_os_error() == Some(libc::ETIME) => return Ok(0),
                        Err(ref err) if err.raw_os_error() == Some(libc::EINTR) => continue,
                        Err(err) => {
                            return Err(err);
                        }
                    }
                }
            }
            None => loop {
                match self.ring.submitter().submit_and_wait(1) {
                    Ok(_) => break,
                    Err(ref err) if err.raw_os_error() == Some(libc::EINTR) => continue,
                    Err(err) => {
                        return Err(err);
                    }
                }
            },
        }
        let mut processed_entry_count = 0usize;
        for completion in self.ring.completion() {
            unsafe {
                let callback = CompletionCallback::from_user_data(completion.user_data());
                callback.call(&completion);
                processed_entry_count += 1;
            }
        }
        self.shared.submission_full_wake_state.wake();
        Ok(processed_entry_count)
    }

    #[inline]
    fn handle(&self) -> Arc<dyn crate::event_queue::HandleImpl> {
        Arc::new(Handle {
            shared: self.shared.clone(),
        })
    }

    fn new_custom_event(&mut self, callback: Box<dyn Fn(usize) + Send + Sync>) -> usize {
        unsafe {
            let mut pipe_fds: [c_int; 2] = [-1, -1];
            libc_call!(libc::pipe2(pipe_fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK))
                .expect("failed to create pipe for event");
            let pipe_read = ScopedFd::from_raw(pipe_fds[0]);
            let pipe_write = ScopedFd::from_raw(pipe_fds[1]);
            let pipe_read_raw = pipe_read.as_raw();
            let key = pipe_write.as_raw() as usize;
            let registered_event = RegisteredEvent {
                pipe_write,
                _completer: Box::pin(CustomEventCompleter::new(callback, pipe_read)),
            };
            let completer_ptr: *const CustomEventCompleter = &*registered_event._completer;
            self.shared.events.write().insert(key, registered_event);
            self.ring
                .submission()
                .push(
                    &opcode::PollAdd::new(io_uring::types::Fd(pipe_read_raw), libc::POLLIN as u32)
                        .multi(true)
                        .build()
                        .user_data(completer_ptr as usize as u64),
                )
                .unwrap();
            key
        }
    }

    fn wake(&self) -> io::Result<()> {
        todo!()
    }
}

impl EventQueueImplExt for IoUring {
    #[inline]
    fn kind(&self) -> EventQueueKind {
        EventQueueKind::IoUring(self)
    }

    #[inline]
    fn kind_mut(&mut self) -> EventQueueKindMut {
        EventQueueKindMut::IoUring(self)
    }
}

impl HandleImpl for Handle {
    fn enqueue_custom_event(&self, key: usize, data: usize) -> io::Result<()> {
        let events = self.shared.events.read();
        let event = events.get(&key).expect("invalid event key");
        unsafe {
            let userdata_buffer = (data as u64).to_ne_bytes();
            libc_call!(libc::write(
                event.pipe_write.as_raw(),
                userdata_buffer.as_ptr() as _,
                userdata_buffer.len()
            ))?;
        }
        Ok(())
    }

    fn wake(&self) -> io::Result<()> {
        unsafe {
            let write_buffer = 1u64.to_ne_bytes();
            libc_call!(libc::write(
                self.shared.wake_eventfd.as_raw(),
                write_buffer.as_ptr() as _,
                write_buffer.len()
            ))?;
            Ok(())
        }
    }
}
