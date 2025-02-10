use std::sync::Arc;

use crate::{api::Api, Instance, Interpreter};

/// A memoized initialization phase of a runtime
///
/// In addition to caching the runtime [`Interpreter`] own code compilation, a particular initialization phase of a set of runtime
/// instances (e.g. a particular rule) can be cached and instantiated as many times as necessary. This allows new rule
/// invocations to start executing almost instantly, with only a memcpy as overhead instead of going through importing
/// and interpreting any source files each time.
#[derive(Clone)]
pub struct Template(Arc<_Template>);

struct _Template {
    _interpreter: Interpreter,
    _memory: Vec<u8>,
}

pub struct Builder<A: Api> {
    interpreter: Interpreter,
    instance: Instance<A>,
}

impl Template {
    pub async fn builder<A: Api>(interpreter: &Interpreter, api: A) -> anyhow::Result<Builder<A>> {
        Builder::new(interpreter, api).await
    }

    pub fn instantiate<A: Api>(&self, _api: A) -> anyhow::Result<Instance<A>> {
        unimplemented!()
    }
}

impl<A: Api> Builder<A> {
    pub async fn new(interpreter: &Interpreter, api: A) -> anyhow::Result<Self> {
        let instance = Instance::builder(interpreter, api)?.build().await?;

        Ok(Builder {
            interpreter: interpreter.clone(),
            instance,
        })
    }

    pub fn capture(mut self) -> Result<Template, CaptureError> {
        let memory = self.instance.primary_memory().to_vec();

        Ok(Template(Arc::new(_Template {
            _interpreter: self.interpreter,
            _memory: memory,
        })))
    }
}

pub enum CaptureError {}
