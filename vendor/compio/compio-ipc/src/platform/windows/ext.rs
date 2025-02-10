use std::{io, os::windows::io::RawHandle};

pub trait MessageChannelExt: Sized {
    /// Creates a [`MessageChannel`](crate::MessageChannel) from an existing named pipe
    ///
    /// The pipe must be configured for overlapped IO and in message mode in both directions
    unsafe fn from_handle(handle: RawHandle) -> io::Result<Self>;
}
