use cpython::{ObjectProtocol, PyErr, PyObject, PyString, Python, PythonObject, ToPyObject};
use thiserror::Error;

use cealn_runtime_data::{Error as ErrorData, InvocationResult, PythonError};

#[derive(Error, Debug)]
pub enum Error {
    #[error("python exception: {0:?}")]
    Python(PyErr),
}

pub fn serializable_result(x: Result<PyString, Error>) -> Result<PyString, Vec<u8>> {
    match x {
        Ok(x) => Ok(x),
        Err(err) => {
            let error_bytes =
                serde_json::to_vec(&InvocationResult::<()>::Err(cealn_runtime_data::Error::from(err))).unwrap();
            Err(error_bytes)
        }
    }
}

impl From<Error> for ErrorData {
    fn from(err: Error) -> ErrorData {
        match err {
            Error::Python(mut err) => {
                let guard = Python::acquire_gil();
                let python = guard.python();
                let instance = err.instance(python);
                let class = err.get_type(python).name(python).into_owned();
                let message = instance
                    .str(python)
                    .map(|x| x.to_string_lossy(python).into_owned())
                    .unwrap_or_else(|_| "[str failed for Python error]".to_owned());
                let traceback = err
                    .ptraceback
                    .as_ref()
                    .and_then(|traceback| format_traceback(python, traceback).ok());
                ErrorData::Python(PythonError {
                    class,
                    message,
                    traceback,
                })
            }
        }
    }
}

// We have to implement this manually since `PyErr` doesn't implement `std::error::Error`
impl From<PyErr> for Error {
    fn from(err: PyErr) -> Error {
        Error::Python(err)
    }
}

fn format_traceback(python: Python, traceback: &PyObject) -> Result<String, PyErr> {
    let traceback_module = python.import("traceback")?;
    let stack_summary = traceback_module.call(python, "extract_tb", (traceback,), None)?;
    let string_array = stack_summary.call_method(python, "format", cpython::NoArgs, None)?;
    let string = ""
        .to_py_object(python)
        .into_object()
        .call_method(python, "join", (string_array,), None)?
        .str(python)?
        .to_string_lossy(python)
        .into_owned();
    Ok(string)
}
