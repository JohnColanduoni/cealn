pub(crate) mod ext;

use std::io;

use crate::event_queue::EventQueueImpl;

use self::ext::EventQueueImplExt;

pub(crate) fn default_event_queue() -> io::Result<Box<dyn EventQueueImpl>> {
    let queue = BrowserEventQueue::new();
    Ok(Box::new(queue))
}

pub(crate) struct BrowserEventQueue {}

impl BrowserEventQueue {
    fn new() -> BrowserEventQueue {
        BrowserEventQueue {}
    }
}

impl EventQueueImpl for BrowserEventQueue {
    fn poll(&self, timeout: Option<std::time::Duration>) -> io::Result<usize> {
        todo!()
    }

    fn handle(&self) -> std::sync::Arc<dyn crate::event_queue::HandleImpl> {
        todo!()
    }

    fn new_custom_event(&mut self, callback: Box<dyn Fn(usize) + Send + Sync>) -> usize {
        todo!()
    }
}

impl EventQueueImplExt for BrowserEventQueue {}
