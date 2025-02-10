pub(crate) mod ext;

use std::io;

use crate::{event_queue::EventQueueImpl, event_queue::EventQueueFactory, kqueue::KQueue};

pub(crate) fn default_event_queue() -> io::Result<Box<dyn EventQueueImpl>> {
    let kqueue = KQueue::new()?;
    Ok(Box::new(kqueue))
}

pub(crate) fn default_event_queue_factory() -> io::Result<Box<dyn EventQueueFactory>> {
    Ok(Box::new(crate::kqueue::Options::default()))
}
