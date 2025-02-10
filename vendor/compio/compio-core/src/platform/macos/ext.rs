use std::io;

use crate::kqueue::KQueue;

pub trait EventQueueExt: Sized {
    fn kqueue(&self) -> &KQueue;
}

pub(crate) trait EventQueueImplExt {
    fn kqueue(&self) -> &KQueue;
}

impl EventQueueExt for crate::EventQueue {
    #[inline]
    fn kqueue(&self) -> &KQueue {
        self.imp.kqueue()
    }
}
