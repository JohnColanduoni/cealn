use std::{
    ffi::{CStr, CString},
    io,
    path::Path,
    ptr,
};

#[cfg(unix)]
use std::os::unix::prelude::*;

#[cfg(target_os = "wasi")]
use std::os::wasi::prelude::*;

use cpython::{PyClone, PyDict, PyErr, PyObject, PyResult, Python, ToPyObject};
use pathdiff::diff_paths;
use python3_sys::Py_file_input;

pub fn run_python_file(
    python: Python,
    actual_path: &Path,
    display_path: &Path,
    package_root: &Path,
    package_root_prefix: &str,
    globals: Option<&PyDict>,
) -> PyResult<()> {
    // Figure out what to set __package__ variable to
    let mut package_root_relative_path = diff_paths(actual_path, package_root).expect("invalid package root");
    // Remove filename
    package_root_relative_path.pop();
    let mut package = package_root_relative_path
        .to_str()
        .expect("invalid UTF-8 in path")
        .replace('/', ".");
    package.insert_str(0, package_root_prefix);

    let globals = globals
        .map(|dict| dict.clone_ref(python))
        .unwrap_or_else(|| PyDict::new(python));
    globals
        .set_item(python, "__package__", &package)
        .expect("failed to set __package__");

    let path_cstr = CString::new(actual_path.as_os_str().as_bytes()).expect("null in filename");
    let file = unsafe { libc::fopen(path_cstr.as_ptr(), CStr::from_bytes_with_nul(b"rb\0").unwrap().as_ptr()) };
    if file.is_null() {
        // TODO: return error here
        panic!(
            "failed to open file {:?}: {:?}",
            actual_path,
            io::Error::last_os_error()
        );
    }
    let display_path_cstr = CString::new(display_path.as_os_str().as_bytes()).expect("null in filename");
    let ret = unsafe {
        globals.with_borrowed_ptr(python, |globals| {
            python3_sys::PyRun_FileExFlags(
                file,
                display_path_cstr.as_ptr(),
                Py_file_input,
                globals,
                ptr::null_mut(),
                1,
                ptr::null_mut(),
            )
        })
    };

    if ret.is_null() {
        Err(PyErr::fetch(python))
    } else {
        // Ensure we cleanup return value
        let _output = unsafe { PyObject::from_owned_ptr(python, ret) };
        Ok(())
    }
}
