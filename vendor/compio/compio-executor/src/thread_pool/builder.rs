use std::{io, sync::Arc};

use compio_core::event_queue::EventQueueFactory;

use super::ThreadPool;

pub struct Builder {
    pub(super) worker_count: usize,
    pub(super) thread_name: Box<dyn FnMut(usize) -> String + Send + 'static>,
    pub(super) wrap_start: Option<Arc<dyn Fn(usize, Box<dyn FnOnce()>) + Send + Sync + 'static>>,
    pub(super) event_queue_factory: Option<Box<dyn EventQueueFactory>>,
}

impl Builder {
    #[inline]
    pub fn new() -> Builder {
        Builder {
            worker_count: num_cpus::get(),
            thread_name: Box::new(default_thread_name),
            wrap_start: None,
            event_queue_factory: None,
        }
    }

    pub fn worker_count(&mut self, worker_count: usize) -> &mut Self {
        self.worker_count = worker_count;
        self
    }

    pub fn thread_name<F>(&mut self, f: F) -> &mut Self
    where
        F: FnMut(usize) -> String + Send + 'static,
    {
        self.thread_name = Box::new(f);
        self
    }

    pub fn event_queue<F>(&mut self, f: F) -> &mut Self
    where
        F: EventQueueFactory,
    {
        self.event_queue_factory = Some(Box::new(f));
        self
    }

    pub fn wrap_start<F>(&mut self, f: F) -> &mut Self
    where
        F: Fn(usize, Box<dyn FnOnce()>) + Send + Sync + 'static,
    {
        self.wrap_start = Some(Arc::new(f));
        self
    }

    pub fn build(&mut self) -> io::Result<ThreadPool> {
        ThreadPool::build(self)
    }
}

fn default_thread_name(index: usize) -> String {
    format!("compio-worker-{:02}", index)
}
