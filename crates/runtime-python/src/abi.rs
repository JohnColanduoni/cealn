use std::mem;

use cealn_runtime_data::DataEncoding;
use cpython::{PyString, Python};
use serde::de::DeserializeOwned;

#[repr(C)]
struct OutputSlice {
    data: *const u8,
    length: usize,
    encoding: DataEncoding,
    backing: OutputSliceBacking,
}

enum OutputSliceBacking {
    Python(PyString),
    Rust(Vec<u8>),
}

#[repr(C)]
struct InputSlice {
    data: *mut u8,
    length: usize,
    capacity: usize,
}

impl Drop for InputSlice {
    fn drop(&mut self) {
        unsafe {
            let buffer = Vec::from_raw_parts(self.data, self.length, self.capacity);
            mem::drop(buffer);
        }
    }
}

#[macro_export]
macro_rules! json_entry_point {
    (fn $name:ident($python:ident : Python) -> $ret:ty $body:block ) => {
        #[no_mangle]
        pub extern "C" fn $name() -> usize {
            let guard = Python::acquire_gil();
            let $python = guard.python();

            let data: ::std::result::Result<::cpython::PyString, Vec<u8>> = $body;

            crate::abi::to_output_slice($python, data)
        }
    };
    (fn $name:ident($python:ident : Python, $arg:ident : $arg_ty:ty) -> $ret:ty $body:block ) => {
        #[no_mangle]
        pub extern "C" fn $name(input_slice: usize) -> usize {
            let guard = Python::acquire_gil();
            let $python = guard.python();

            let $arg = unsafe { crate::abi::from_input_slice(input_slice) };

            let data: ::std::result::Result<::cpython::PyString, Vec<u8>> = $body;

            crate::abi::to_output_slice($python, data)
        }
    };
}

#[doc(hidden)]
pub unsafe fn from_input_slice<T>(slice_ptr: usize) -> T
where
    T: DeserializeOwned,
{
    unsafe {
        let box_slice = Box::from_raw(slice_ptr as *mut InputSlice);
        let slice = std::slice::from_raw_parts(box_slice.data as *const u8, box_slice.length);
        let data: T = serde_json::from_slice(slice).unwrap();
        mem::drop(box_slice);
        data
    }
}

#[doc(hidden)]
pub fn to_output_slice(python: Python, result: Result<PyString, Vec<u8>>) -> usize {
    let (data, length, encoding, backing) = match result {
        Ok(string) => match string.data(python) {
            cpython::PyStringData::Utf8(data) => (
                data.as_ptr(),
                data.len(),
                DataEncoding::Utf8,
                OutputSliceBacking::Python(string),
            ),
            cpython::PyStringData::Latin1(data) => (
                data.as_ptr(),
                data.len(),
                DataEncoding::Latin1,
                OutputSliceBacking::Python(string),
            ),
            _ => todo!(),
        },
        Err(data) => (
            data.as_ptr(),
            data.len(),
            DataEncoding::Utf8,
            OutputSliceBacking::Rust(data),
        ),
    };
    let slice = Box::new(OutputSlice {
        data,
        length,
        encoding,
        backing,
    });
    Box::into_raw(slice) as usize
}

#[no_mangle]
pub extern "C" fn cealn_alloc_input_buffer(capacity: usize) -> usize {
    let (data, length, capacity) = Vec::with_capacity(capacity).into_raw_parts();
    let slice = Box::new(InputSlice { data, length, capacity });
    Box::into_raw(slice) as usize
}

#[no_mangle]
pub extern "C" fn cealn_free_output_buffer(ptr: usize) {
    unsafe {
        let slice: Box<OutputSlice> = Box::from_raw(ptr as *mut OutputSlice);
        mem::drop(slice);
    }
}
