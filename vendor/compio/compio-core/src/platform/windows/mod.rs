pub mod buffer;
pub mod ext;

use std::io;

use crate::{
    event_queue::{EventQueueFactory, EventQueueImpl},
    iocp::Iocp,
};

pub(crate) fn default_event_queue() -> io::Result<Box<dyn EventQueueImpl>> {
    let iocp = Iocp::new()?;
    Ok(Box::new(iocp))
}

pub(crate) fn default_event_queue_factory() -> io::Result<Box<dyn EventQueueFactory>> {
    Ok(Box::new(crate::iocp::Options::default()))
}
