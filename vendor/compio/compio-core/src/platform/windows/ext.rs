use std::io;

use crate::iocp::Iocp;

pub trait EventQueueImplExt {}

pub trait EventQueueExt: Sized {
    fn as_iocp(&self) -> &Iocp;
}
