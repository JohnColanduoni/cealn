use std::{ffi::CString, io::Read};

use thiserror::Error;
use tracing::debug_span;
use xz2::read::XzDecoder;

use cealn_runtime::{
    interpreter::{self, Options, Spec},
    Interpreter,
};
use cealn_runtime_virt::fs::memory_tar::{self, InMemoryTar};

const PYTHON_RUNTIME_WASM: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/runtime-python.wasm"));

const PYTHON_FS_ARCHIVE_COMPRESSED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/python_fs.tar.xz"));

pub fn make_interpreter(options: Options) -> Result<Interpreter, CreateError> {
    let libs = InMemoryTar::build((&**PYTHON_FS_ARCHIVE).into())?;
    let interpreter = Interpreter::new(
        Spec {
            wasm_module_bin: PYTHON_RUNTIME_WASM.into(),
            static_filesystems: libs.roots(),
            default_environment_variables: vec![
                // We need to add site packages manually to the path becasue we disable the site import due to a bug
                (
                    CString::new("PYTHONPATH").unwrap(),
                    CString::new("/usr/lib/python3.11/site-packages").unwrap(),
                ),
            ],
        },
        options,
    )?;

    Ok(interpreter)
}

lazy_static::lazy_static! {
    static ref PYTHON_FS_ARCHIVE: Vec<u8>  = {
        let span = debug_span!("decompress_python_filesystem", compressed_size = PYTHON_FS_ARCHIVE_COMPRESSED.len());
        let _guard = span.enter();
        let mut decoder = XzDecoder::new(PYTHON_FS_ARCHIVE_COMPRESSED);
        let mut buffer = Vec::new();
        decoder.read_to_end(&mut buffer).expect("failed to decompress embeded filesystem");
        buffer
    };
}

#[derive(Error, Debug)]
pub enum CreateError {
    #[error("failed to create interpreter: {0}")]
    Interpreter(#[from] interpreter::CreateError),
    #[error("failed to create interpreter: {0}")]
    StaticFilesystem(#[from] memory_tar::BuildError),
}
