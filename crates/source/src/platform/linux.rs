use std::{
    collections::{hash_map::Entry, HashMap},
    convert::TryFrom,
    ffi::CString,
    io, mem,
    os::unix::prelude::*,
    path::Path,
    pin::Pin,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    task::{Poll, Waker},
    time::Duration,
};

use anyhow::{Context, Result};
use cealn_core::libc_call;
use futures::prelude::*;
use libc::c_int;
use slab::Slab;

use crate::{entry::SourceEntryMonitor, watcher::WatchDepth};

#[derive(Clone)]
pub(crate) struct WatchPort {
    shared: Arc<_WatchPort>,
}

struct _WatchPort {
    inotify: RawFd,
    interrupt: RawFd,

    nodes: Mutex<HashMap<c_int, Arc<_WatchNode>>>,
    any_change: Mutex<WaitState>,
}

impl Drop for _WatchPort {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.inotify);
            libc::close(self.interrupt);
        }
    }
}

pub(crate) struct WatchNode {
    shared: Arc<_WatchNode>,
    last_observed_sequence_number: AtomicU64,
}

struct _WatchNode {
    port: Arc<_WatchPort>,
    wait_state: Mutex<WaitState>,
    desc: c_int,
    source_monitor: Weak<SourceEntryMonitor>,
}

struct WaitState {
    sequence_number: u64,
    wakers: Slab<Waker>,
}

pub(crate) struct ObserveGuard {
    prior_sequence_number: u64,
    observation_sequence_number: u64,
}

pub(crate) struct AnyChangeObserveGuard {
    observation_sequence_number: u64,
}

impl Drop for _WatchNode {
    fn drop(&mut self) {
        unsafe {
            // FIXME: in the duplicate case, this will remove the descriptor too early
            let _ = libc_call!(libc::inotify_rm_watch(self.port.inotify, self.desc));
            self.port.nodes.lock().unwrap().remove(&self.desc);
        }
    }
}

pub const WATCH_DEPTH: WatchDepth = WatchDepth::One;

// TODO: don't pull this out of our ass
const EVENT_BUFFER_SIZE: usize = 8192;

impl WatchPort {
    pub(crate) fn new() -> Result<WatchPort> {
        let inotify = unsafe { libc_call!(libc::inotify_init1(libc::IN_CLOEXEC))? };
        let interrupt = unsafe {
            libc_call!(libc::eventfd(
                0,
                libc::EFD_CLOEXEC | libc::EFD_SEMAPHORE | libc::EFD_NONBLOCK
            ))?
        };
        Ok(WatchPort {
            shared: Arc::new(_WatchPort {
                inotify,
                interrupt,
                nodes: Default::default(),
                any_change: Mutex::new(WaitState {
                    sequence_number: 0,
                    wakers: Slab::new(),
                }),
            }),
        })
    }

    pub(crate) fn watch(&self, path: &Path, source_monitor: &Arc<SourceEntryMonitor>) -> Result<Option<WatchNode>> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())
            .with_context(|| format!("attempted to watch invalid path {:?}", path))?;

        let watch_mask = libc::IN_ATTRIB
            | libc::IN_CREATE
            | libc::IN_DELETE
            | libc::IN_DELETE_SELF
            | libc::IN_MODIFY
            | libc::IN_MOVE_SELF
            | libc::IN_MOVED_FROM
            | libc::IN_MOVED_TO;
        match unsafe {
            libc_call!(libc::inotify_add_watch(
                self.shared.inotify,
                path_cstr.as_ptr(),
                watch_mask
            ))
        } {
            Ok(desc) => {
                let shared;
                let last_observed_sequence_number;
                {
                    let mut nodes = self.shared.nodes.lock().unwrap();
                    // Certain cases can result in duplicate watch descriptors, handle this smoothly
                    match nodes.entry(desc) {
                        Entry::Occupied(entry) => {
                            shared = entry.get().clone();
                            let wait_state = shared.wait_state.lock().unwrap();
                            last_observed_sequence_number = wait_state.sequence_number;
                        }
                        Entry::Vacant(entry) => {
                            shared = Arc::new(_WatchNode {
                                port: self.shared.clone(),
                                desc,
                                wait_state: Mutex::new(WaitState {
                                    sequence_number: 0,
                                    wakers: Default::default(),
                                }),
                                source_monitor: Arc::downgrade(source_monitor),
                            });
                            entry.insert(shared.clone());
                            last_observed_sequence_number = 0;
                        }
                    }
                };

                Ok(Some(WatchNode {
                    shared,
                    last_observed_sequence_number: AtomicU64::new(last_observed_sequence_number),
                }))
            }
            Err(ref err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(anyhow::Error::from(err).context(format!("failed to watch file {:?}", path))),
        }
    }

    pub(crate) fn poll(&self, timeout: Option<Duration>) -> Result<usize> {
        unsafe {
            let timeout = match timeout {
                Some(timeout) => i32::try_from(timeout.as_millis()).with_context(|| format!("timeout too large"))?,
                None => -1,
            };
            let mut fds = [
                libc::pollfd {
                    fd: self.shared.inotify,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: self.shared.interrupt,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];

            libc_call!(libc::poll(fds.as_mut_ptr(), fds.len() as u64, timeout))
                .with_context(|| format!("inotify poll failed"))?;

            if fds[1].revents & libc::POLLIN != 0 {
                // Interrupted, read one from interrupt and return
                let mut buffer = [0u8; 8];
                libc_call!(libc::read(
                    self.shared.interrupt,
                    buffer.as_mut_ptr() as _,
                    buffer.len()
                ))?;
                return Ok(0);
            }

            let mut event_byte_buffer = [0u8; EVENT_BUFFER_SIZE];
            let read_bytes = match libc_call!(libc::read(
                self.shared.inotify,
                event_byte_buffer.as_mut_ptr() as _,
                event_byte_buffer.len()
            )) {
                Ok(read_bytes) => read_bytes,
                Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => {
                    return Ok(0);
                }
                Err(err) => return Err(anyhow::Error::from(err).context(format!("inotify read failed"))),
            };

            let mut remaining_bytes = &event_byte_buffer[..read_bytes as usize];
            let nodes = self.shared.nodes.lock().unwrap();
            let mut events_processed = 0;
            let mut any_change = false;
            while remaining_bytes.len() >= mem::size_of::<libc::inotify_event>() {
                let event = &*(remaining_bytes.as_ptr() as *const libc::inotify_event);
                tracing::trace!(fd = event.wd, "inotify_event");

                if let Some(node) = nodes.get(&event.wd) {
                    any_change = true;
                    let wakers;
                    let monitor;
                    {
                        let mut wait_state = node.wait_state.lock().unwrap();
                        wait_state.sequence_number += 1;
                        wakers = mem::replace(&mut wait_state.wakers, Slab::new());
                        monitor = node.source_monitor.upgrade();
                    };
                    if let Some(monitor) = monitor {
                        monitor.watch_dir_did_change();
                    }
                    for (_, waker) in wakers.into_iter() {
                        waker.wake();
                    }
                }

                remaining_bytes = &remaining_bytes[(mem::size_of::<libc::inotify_event>() + event.len as usize)..];
                events_processed += 1;
            }

            if any_change {
                let wakers = {
                    let mut wait_state = self.shared.any_change.lock().unwrap();
                    wait_state.sequence_number += 1;
                    mem::replace(&mut wait_state.wakers, Slab::new())
                };
                for (_, waker) in wakers.into_iter() {
                    waker.wake();
                }
            }

            Ok(events_processed)
        }
    }

    pub(crate) fn will_observe_any_change(&self) -> AnyChangeObserveGuard {
        let wait_state = self.shared.any_change.lock().unwrap();
        AnyChangeObserveGuard {
            observation_sequence_number: wait_state.sequence_number,
        }
    }

    pub(crate) fn wait_for_any_change(&self, guard: AnyChangeObserveGuard) -> WaitForAnyChange {
        WaitForAnyChange {
            shared: self.shared.clone(),
            observation_sequence_number: guard.observation_sequence_number,
            waker_index: None,
        }
    }

    pub fn any_change_guard_check_dirty(&self, guard: &mut AnyChangeObserveGuard) -> Result<bool> {
        let mut wait_state = self.shared.any_change.lock().unwrap();
        if wait_state.sequence_number > guard.observation_sequence_number {
            guard.observation_sequence_number = wait_state.sequence_number;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

impl WatchNode {
    pub(crate) fn duplicate(&self) -> Self {
        WatchNode {
            shared: self.shared.clone(),
            last_observed_sequence_number: AtomicU64::new(0),
        }
    }

    pub(crate) fn has_changed(&self) -> bool {
        let wait_state = self.shared.wait_state.lock().unwrap();
        let last_observed_sequence_number = self.last_observed_sequence_number.load(Ordering::SeqCst);
        wait_state.sequence_number > last_observed_sequence_number
    }

    pub(crate) fn will_observe(&self) -> ObserveGuard {
        let wait_state = self.shared.wait_state.lock().unwrap();
        ObserveGuard {
            prior_sequence_number: self.last_observed_sequence_number.load(Ordering::SeqCst),
            observation_sequence_number: wait_state.sequence_number,
        }
    }
}

impl ObserveGuard {
    pub(crate) fn commit(&self, node: &WatchNode) {
        node.last_observed_sequence_number.compare_exchange(
            self.prior_sequence_number,
            self.observation_sequence_number,
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }
}

pub(crate) struct WaitForAnyChange {
    shared: Arc<_WatchPort>,
    observation_sequence_number: u64,
    waker_index: Option<usize>,
}

impl Future for WaitForAnyChange {
    type Output = Result<()>;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut wait_state = this.shared.any_change.lock().unwrap();

        if wait_state.sequence_number > this.observation_sequence_number {
            return Poll::Ready(Ok(()));
        }

        if !this
            .waker_index
            .and_then(|waker_index| wait_state.wakers.get(waker_index))
            .map(|waker| waker.will_wake(cx.waker()))
            .unwrap_or(false)
        {
            this.waker_index = Some(wait_state.wakers.insert(cx.waker().clone()));
        }

        Poll::Pending
    }
}
