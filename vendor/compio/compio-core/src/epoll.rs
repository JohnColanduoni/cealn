pub(crate) mod event_queue;
pub(crate) mod registration;

pub use self::{
    event_queue::Epoll,
    registration::{Registration, WaitForRead, WaitForWrite},
};
