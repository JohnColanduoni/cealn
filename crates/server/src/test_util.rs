use std::{path::PathBuf, sync::Once};

use cealn_runtime::{interpreter::Options, Interpreter};

use crate::executor::{self, Executor};

lazy_static::lazy_static! {
    static ref INTERPRETER: Interpreter = {
        let mut cache_config_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        cache_config_path.push("test_cache_config.toml");
        cealn_runtime_python_embed::make_interpreter(Options {
            cache_config_file: Some(cache_config_path),
        }).unwrap()
    };
}

pub(crate) fn python_interpreter_for_testing() -> &'static Interpreter {
    &*INTERPRETER
}

pub(crate) fn executor_for_testing() -> Executor {
    Executor::new(executor::Options {
        thread_pool_concurrency: Some(1),
    })
    .unwrap()
}
