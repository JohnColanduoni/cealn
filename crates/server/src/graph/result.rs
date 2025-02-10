use std::{
    convert::Infallible,
    fmt,
    ops::{ControlFlow, FromResidual, Try},
    sync::Arc,
};

use cealn_protocol::{event::BuildEvent, query::Query};

#[derive(Debug)]
pub struct QueryResult<T> {
    shared: Arc<Shared<T>>,
}

#[derive(Debug)]
struct Shared<T> {
    output: Result<T, QueryError>,
    events: Vec<BuildEvent>,

    run_id: usize,
}

impl<T> QueryResult<T> {
    pub(crate) fn new(output: anyhow::Result<T>, events: Vec<BuildEvent>, run_id: usize) -> Self {
        QueryResult {
            shared: Arc::new(Shared {
                events,
                output: output.map_err(QueryError::new),
                run_id,
            }),
        }
    }

    #[inline]
    pub fn output(&self) -> &Result<T, QueryError> {
        &self.shared.output
    }

    pub(crate) fn run_id(&self) -> usize {
        self.shared.run_id
    }

    pub fn output_ref(&self) -> anyhow::Result<&T> {
        match &self.shared.output {
            Ok(output) => Ok(output),
            Err(error) => Err(anyhow::Error::new(error.clone())),
        }
    }
}

impl<T> Clone for QueryResult<T> {
    #[inline]
    fn clone(&self) -> Self {
        QueryResult {
            shared: self.shared.clone(),
        }
    }
}

#[derive(Clone)]
pub struct QueryError {
    shared: Arc<_QueryError>,
}

struct _QueryError {
    inner: anyhow::Error,
}

impl QueryError {
    pub fn new(cause: anyhow::Error) -> Self {
        QueryError {
            shared: Arc::new(_QueryError { inner: cause }),
        }
    }

    pub fn inner(&self) -> &anyhow::Error {
        &self.shared.inner
    }
}

impl std::error::Error for QueryError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.shared.inner)
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&self.shared.inner, f)
    }
}

impl fmt::Debug for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.shared.inner, f)
    }
}
