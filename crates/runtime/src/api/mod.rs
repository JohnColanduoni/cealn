pub(crate) mod wasi;

pub mod types {
    pub use super::wasi::types::*;
}

pub use self::wasi::{Handle, HandleRights, InjectFdError, Result};

use std::{ffi::CString, sync::Arc};

use thiserror::Error;

use crate::api::wasi::WasiCtx;

/// An implementation of the API imported by [`Interpreter`]s
///
/// All interactions with the outside world from the runtime go through an implementer of this trait.
// Allow unused variables in our stub implementations
#[allow(unused_variables)]
pub trait Api: ApiDispatch + Clone + 'static {
    /// Attaches the API to a context
    ///
    /// This gives the API a chance to obtain a reference to its context so it can do things like inject file
    /// handles.
    ///
    /// This is called once when the [`Api`] is first attached to an [`crate::Instance`]. It is not called again when
    /// forking.
    fn init(&self, ctx: &WasiCtx) {}

    /// Forks the `Api` state
    ///
    /// `Api`s can implement this to allow the [`ApiContext`] to be forked so it can be used in a [`crate::Template`].
    /// The newly returned instance should not share any state with the previous one, but should not cause any changes
    /// to the [`crate::Instance`].
    fn fork(&self) -> std::result::Result<Self, ForkError> {
        Err(ForkError::NotSupported)
    }
}

pub trait ApiDispatch: Send {
    /// The process arguments
    ///
    /// Note that like in most native ABIs, this is only evaluated once at launch. Changes to the return value will
    /// not be seen by the process, even if it asks for the arguments again (which it generally won't
    /// anyway).
    fn args(&self) -> &[CString] {
        &[]
    }

    /// The environment variables provided by the API.
    ///
    /// These will override any interpreter-provided default environment variables.
    ///
    /// Note that like in most native ABIs, this is only evaluated once at launch. Changes to the return value will
    /// not be seen by the process, even if it asks for the environment variables again (which it generally won't
    /// anyway).
    fn envs(&self) -> &[(CString, CString)] {
        &[]
    }

    /// Filesystems to be mounted into the system.
    ///
    /// Note that changes to this variable will not be observed after launch. If changes need to be made to the set
    /// of available filesystems, use a `MountFs`.
    fn filesystems(&self) -> &[(String, Arc<dyn Handle>)] {
        &[]
    }

    /// Value (in nanoseconds) to return when the realtime clock is requested
    fn realtime_clock(&self) -> u64 {
        0
    }

    /// Value (in nanoseconds) to return when the monotonic clock is requested
    fn monotonic_clock(&self) -> u64 {
        0
    }
}

#[derive(Error, Debug)]
pub enum InitError {
    #[error("failed to initialize WASI api: {0}")]
    Wasi(#[from] wasi::InitError),
}

#[derive(Error, Debug)]
pub enum ForkError {
    #[error("this Api does not support forking")]
    NotSupported,
}
