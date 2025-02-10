use std::{env, ffi::CString, fs, io, sync::Once};

use cealn_runtime::{
    api::{types, Api, ApiDispatch},
    interpreter, Instance, Interpreter,
};
use cealn_runtime_python_embed::make_interpreter;
use cealn_runtime_virt::fs::print::PrintHandle;
use cealn_test_util::prep;

fn interpreter_factory() -> Interpreter {
    let mut cache_config_file = env::temp_dir();
    cache_config_file.push("cealn-runtime-tests-cache-python.toml");
    fs::write(
        &cache_config_file,
        r#"
    [cache]
    enabled = true
    "#,
    )
    .unwrap();

    make_interpreter(interpreter::Options {
        cache_config_file: Some(cache_config_file),
    })
    .expect("failed to create interpreter")
}

static MAKE_INTERPRETER: Once = Once::new();
static mut SHARED_INTERPRETER: Option<Interpreter> = None;

// Interpreter creation is not fast (at least a few seconds, even with unoptimized code and caching) so
// we only make it once per test suite run.
fn get_interpreter() -> Interpreter {
    unsafe {
        MAKE_INTERPRETER.call_once(|| {
            SHARED_INTERPRETER = Some(interpreter_factory());
        });
        SHARED_INTERPRETER.as_ref().cloned().unwrap()
    }
}

#[test]
fn build_interpreter() {
    prep();

    let _interpreter = get_interpreter();
}

#[derive(Clone)]
struct DummyApi;

impl Api for DummyApi {}

impl ApiDispatch for DummyApi {
    fn envs(&self) -> &[(CString, CString)] {
        &*DEFAULT_ENVS
    }
}

lazy_static::lazy_static! {
    static ref DEFAULT_ENVS: Vec<(CString, CString)> = vec![
        (CString::new("RUST_BACKTRACE").unwrap(), CString::new("1").unwrap()),
    ];
}

#[test]
fn initialize_module() {
    prep();

    let interpreter = get_interpreter();
    let builder = Instance::builder(&interpreter, DummyApi).unwrap();

    // Inject stdout/stderr pipes
    builder
        .wasi_ctx()
        .inject_fd(PrintHandle::new(io::stdout()).to_handle(), Some(types::Fd::from(1)))
        .unwrap();
    builder
        .wasi_ctx()
        .inject_fd(PrintHandle::new(io::stderr()).to_handle(), Some(types::Fd::from(2)))
        .unwrap();

    let _instance = builder.build().map_err(|err| err.to_string()).unwrap();
}
