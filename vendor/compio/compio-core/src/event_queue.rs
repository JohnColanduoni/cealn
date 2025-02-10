use std::{
    borrow::Borrow,
    cell::RefCell,
    io,
    marker::PhantomData,
    mem::{self, ManuallyDrop},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use scoped_tls::scoped_thread_local;
use static_assertions::assert_impl_all;
use tracing::{debug, error, trace_span};

use crate::platform::ext::EventQueueImplExt;

pub struct EventQueue {
    pub(crate) imp: Box<dyn EventQueueImpl>,
    event_queue_id: usize,
}

#[derive(Clone)]
pub struct Handle {
    pub(crate) imp: Arc<dyn HandleImpl>,
    event_queue_id: usize,
}

#[derive(Clone)]
pub struct CustomEvent {
    event_queue_id: usize,
    key: usize,
}

pub trait EventQueueFactory: 'static {
    fn new(&self) -> io::Result<EventQueue>;

    fn supports_completion_stealing(&self) -> bool {
        false
    }
}

impl EventQueue {
    /// Creates a new `EventQueue` with the default backend and settings for the current platform
    ///
    /// Use platform-specific extensions to customize configuration of the `EventQueue`.
    #[inline]
    pub fn new() -> io::Result<EventQueue> {
        let imp = crate::platform::default_event_queue()?;
        Ok(Self::with_imp(imp))
    }

    pub fn default_factory() -> io::Result<Box<dyn EventQueueFactory>> {
        crate::platform::default_event_queue_factory()
    }

    #[inline]
    pub(crate) fn with_imp(imp: Box<dyn EventQueueImpl>) -> EventQueue {
        // We don't care about order here, only uniqueness
        let event_queue_id = NEXT_EVENT_QUEUE_ID.fetch_add(1, Ordering::Relaxed);
        EventQueue { imp, event_queue_id }
    }

    #[inline]
    pub fn poll(&self, timeout: Option<Duration>) -> io::Result<usize> {
        let span = trace_span!("poll", events = tracing::field::Empty);
        let _guard = span.enter();
        match self.imp.poll(timeout) {
            Ok(events) => {
                span.record("events", &events);
                Ok(events)
            }
            Err(err) => {
                error!(error = ?err);
                Err(err)
            }
        }
    }

    #[inline]
    pub fn poll_mut(&mut self, timeout: Option<Duration>) -> io::Result<usize> {
        let span = trace_span!("poll_mut", events = tracing::field::Empty);
        let _guard = span.enter();
        match self.imp.poll_mut(timeout) {
            Ok(events) => {
                span.record("events", &events);
                Ok(events)
            }
            Err(err) => {
                error!(error = ?err);
                Err(err)
            }
        }
    }

    #[inline]
    pub fn handle(&self) -> Handle {
        Handle {
            imp: self.imp.handle(),
            event_queue_id: self.event_queue_id,
        }
    }

    /// Creates a new [`CustomEvent`](CustomEvent) that triggers the desired callback
    ///
    ///
    #[inline]
    pub fn new_custom_event<F>(&mut self, callback: F) -> CustomEvent
    where
        F: Fn(usize) + Send + Sync + 'static,
    {
        let key = self.imp.new_custom_event(Box::new(callback));
        CustomEvent {
            event_queue_id: self.event_queue_id,
            key,
        }
    }

    #[inline]
    pub fn wake(&self) -> io::Result<()> {
        self.imp.wake()
    }

    #[inline]
    pub fn set_current(&self) -> CurrentEventQueueGuard {
        unsafe {
            let old = mem::replace(&mut CURRENT_EVENT_QUEUE, CurrentEventQueueStorage::Shared(self));
            CurrentEventQueueGuard {
                old: ManuallyDrop::new(old),
                _phantom: PhantomData,
            }
        }
    }

    #[inline]
    pub fn set_current_mut(&mut self) -> CurrentEventQueueMutGuard {
        unsafe {
            let old = mem::replace(
                &mut CURRENT_EVENT_QUEUE,
                CurrentEventQueueStorage::Unique(RefCell::new(self)),
            );
            CurrentEventQueueMutGuard {
                old: ManuallyDrop::new(old),
                _phantom: PhantomData,
            }
        }
    }

    #[inline]
    pub fn with_current<T, F>(f: F) -> T
    where
        F: FnOnce(&EventQueue) -> T,
    {
        unsafe {
            match &CURRENT_EVENT_QUEUE {
                CurrentEventQueueStorage::None => panic!("compio_core::EventQueue is not set for the current thread"),
                CurrentEventQueueStorage::Shared(ptr) => f(&**ptr),
                CurrentEventQueueStorage::Unique(cell) => {
                    let ptr = cell.borrow();
                    f(&**ptr)
                }
            }
        }
    }

    #[inline]
    pub fn with_current_mut<T, F>(f: F) -> T
    where
        F: FnOnce(&mut EventQueue) -> T,
    {
        unsafe {
            match &CURRENT_EVENT_QUEUE {
                CurrentEventQueueStorage::None => panic!("compio_core::EventQueue is not set for the current thread"),
                CurrentEventQueueStorage::Shared(ptr) => panic!("current event queue is not bound uniquely"),
                CurrentEventQueueStorage::Unique(cell) => {
                    let mut ptr = cell.borrow();
                    f(&mut **ptr)
                }
            }
        }
    }

    #[inline]
    pub fn with_current_mut_or_else<T, F, G>(f: F, g: G) -> T
    where
        F: FnOnce(&mut EventQueue) -> T,
        G: FnOnce(&EventQueue) -> T,
    {
        unsafe {
            match &CURRENT_EVENT_QUEUE {
                CurrentEventQueueStorage::None => panic!("compio_core::EventQueue is not set for the current thread"),
                CurrentEventQueueStorage::Shared(ptr) => g(&**ptr),
                CurrentEventQueueStorage::Unique(cell) => {
                    let mut ptr = cell.borrow_mut();
                    f(&mut **ptr)
                }
            }
        }
    }
}

impl Handle {
    /// Causes the callback associated with a previously registered `CustomEvent` to be triggered by a polling thread
    #[inline]
    pub fn enqueue_custom_event(&self, custom_event: &CustomEvent, data: usize) -> io::Result<()> {
        if custom_event.event_queue_id != self.event_queue_id {
            panic!("provided CustomEvent was not created on this EventQueue instance");
        }
        self.imp.enqueue_custom_event(custom_event.key, data)
    }

    #[inline]
    pub fn wake(&self) -> io::Result<()> {
        self.imp.wake()
    }
}

pub(crate) trait EventQueueImpl: Send + Sync + EventQueueImplExt + 'static {
    fn poll(&self, timeout: Option<Duration>) -> io::Result<usize>;

    fn poll_mut(&mut self, timeout: Option<Duration>) -> io::Result<usize>;

    fn handle(&self) -> Arc<dyn HandleImpl>;

    fn new_custom_event(&mut self, callback: Box<dyn Fn(usize) + Send + Sync>) -> usize;

    fn wake(&self) -> io::Result<()>;

    // On Windows, the only backend is the IOCP
    #[cfg(target_os = "windows")]
    fn as_iocp(&self) -> &crate::iocp::Iocp;
}

pub(crate) trait HandleImpl: Send + Sync + 'static {
    fn enqueue_custom_event(&self, key: usize, data: usize) -> io::Result<()>;

    fn wake(&self) -> io::Result<()>;
}

pub struct CurrentEventQueueGuard<'a> {
    old: ManuallyDrop<CurrentEventQueueStorage>,
    _phantom: PhantomData<&'a EventQueue>,
}

pub struct CurrentEventQueueMutGuard<'a> {
    old: ManuallyDrop<CurrentEventQueueStorage>,
    _phantom: PhantomData<&'a mut EventQueue>,
}

impl<'a> Drop for CurrentEventQueueGuard<'a> {
    fn drop(&mut self) {
        unsafe {
            CURRENT_EVENT_QUEUE = ManuallyDrop::take(&mut self.old);
        }
    }
}

impl<'a> Drop for CurrentEventQueueMutGuard<'a> {
    fn drop(&mut self) {
        unsafe {
            CURRENT_EVENT_QUEUE = ManuallyDrop::take(&mut self.old);
        }
    }
}

static NEXT_EVENT_QUEUE_ID: AtomicUsize = AtomicUsize::new(1);

#[thread_local]
static mut CURRENT_EVENT_QUEUE: CurrentEventQueueStorage = CurrentEventQueueStorage::None;

enum CurrentEventQueueStorage {
    None,
    Shared(*const EventQueue),
    Unique(RefCell<*mut EventQueue>),
}

assert_impl_all!(EventQueue: Send, Sync);
assert_impl_all!(Handle: Send, Sync);
assert_impl_all!(CustomEvent: Send, Sync);

#[cfg(test)]
mod tests {
    use crate::EventQueue;

    #[test]
    fn create_event_queue() {
        let _queue = EventQueue::new().unwrap();
    }
}
