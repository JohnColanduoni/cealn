use std::{borrow::Cow, ffi::CString, path::PathBuf, sync::Arc};

use thiserror::Error;
use wasmtime::{Config, Engine, Module, OptLevel, Strategy};

use crate::api::Handle;

/// A compiled WebAssembly binary used for a class of runtime [`crate::Instance`]s
///
/// At its core each runtime instance has a WebAssembly binary containing the natively executable code that runs in the
/// sandbox. For example, the Python runtime's [`Interpreter`] is the compiled CPython interpreter along with bindings
/// to cealn-specific APIs. Then individual rules turn into [`crate::Template`]s, and rule invocations
/// become [`crate::Instance`]s.
#[derive(Clone)]
pub struct Interpreter(Arc<_Interpreter>);

struct _Interpreter {
    // TODO: consider whether we want to share this engine
    engine: Engine,
    module: Module,

    static_filesystems: Vec<(String, Arc<dyn StaticHandle>)>,
    default_environment_variables: Vec<(CString, CString)>,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub cache_config_file: Option<PathBuf>,
}

/// A specification for an interpreter environment
pub struct Spec<'a> {
    pub wasm_module_bin: Cow<'a, [u8]>,
    /// Static filesystems that will be injected into all interpreter instances
    pub static_filesystems: Vec<(String, Arc<dyn StaticHandle>)>,
    pub default_environment_variables: Vec<(CString, CString)>,
}

/// A filesystem handle designed to be instantiated into multiple instances
///
/// Unlike normal `Handle`s, `StaticHandle`s should not share any state or reflect changes made through any other
/// handles.
pub trait StaticHandle: Send + Sync {
    fn instantiate(&self) -> Arc<dyn Handle>;
}

impl Interpreter {
    #[tracing::instrument("Interpreter::new", level = "info", skip(spec))]
    pub fn new<'a>(spec: Spec<'a>, options: Options) -> Result<Self, CreateError> {
        let mut config = Config::new();

        config.async_support(true);

        // We usually create very few interpreters and use them a lot, so use higher quality but slower codegen
        config
            .strategy(Strategy::Cranelift)
            .cranelift_opt_level(OptLevel::Speed);

        if let Some(cache_config_file) = &options.cache_config_file {
            config.cache_config_load(cache_config_file).map_err(CreateError::Wasm)?;
        }

        let engine = Engine::new(&config).map_err(CreateError::Wasm)?;

        let module = Module::from_binary(&engine, &spec.wasm_module_bin).map_err(CreateError::Wasm)?;

        Ok(Interpreter(Arc::new(_Interpreter {
            engine,
            module,
            static_filesystems: spec.static_filesystems,
            default_environment_variables: spec.default_environment_variables,
        })))
    }

    pub(crate) fn engine(&self) -> &Engine {
        &self.0.engine
    }

    pub(crate) fn module(&self) -> &Module {
        &self.0.module
    }

    pub(crate) fn static_filesystems(&self) -> &[(String, Arc<dyn StaticHandle>)] {
        &self.0.static_filesystems
    }

    pub(crate) fn default_environment_variables(&self) -> &[(CString, CString)] {
        &self.0.default_environment_variables
    }
}

#[derive(Error, Debug)]
pub enum CreateError {
    #[error("error compiling WASM module: {0}")]
    Wasm(anyhow::Error),
}
