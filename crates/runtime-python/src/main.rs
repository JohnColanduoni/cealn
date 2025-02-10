#![feature(panic_info_message)]
#![feature(core_intrinsics)]

use core::arch::wasm32;
use std::{ffi::CStr, mem, panic::PanicInfo};

use cpython::Python;
use python3_sys as ffi;

use cealn_runtime_python::init_patches;

fn main() {
    init_patches();
    init_python();
    init_env();
}

fn init_python() {
    unsafe {
        std::panic::set_hook(Box::new(panic_hook));

        let python_home = CStr::from_bytes_with_nul(b"/usr\0").unwrap();
        let mut size: libc::size_t = 0;
        let python_home_wchar = ffi::Py_DecodeLocale(python_home.as_ptr(), &mut size);
        ffi::Py_SetPythonHome(python_home_wchar as _);

        let mut pre_config = mem::MaybeUninit::<ffi::PyPreConfig>::zeroed();
        ffi::PyPreConfig_InitPythonConfig(pre_config.as_mut_ptr());
        let mut pre_config = pre_config.assume_init();
        pre_config.parse_argv = 1;
        pre_config.utf8_mode = 1;
        pre_config.dev_mode = if cfg!(debug_assertions) { 1 } else { 0 };
        // TODO: set debug allocator

        let status = ffi::Py_PreInitialize(&pre_config);
        assert_status(status);

        let mut config = mem::MaybeUninit::<ffi::PyConfig>::zeroed();
        ffi::PyConfig_InitPythonConfig(config.as_mut_ptr());
        let mut config = config.assume_init();
        // Disable hash randomization, as it would induce non-determinism
        config.use_hash_seed = 1;
        config.hash_seed = 0;
        // They're fake anyway
        config.install_signal_handlers = 0;
        // None of the paths we import from are writable
        config.write_bytecode = 0;
        // Output stdout/stderr buffering behaves badly in this environment
        config.buffered_stdio = 0;
        //config.verbose = if cfg!(debug_assertions) { 1 } else { 0 };
        // Faulthandler requires some libc functions we don't implement
        config.faulthandler = 0;

        // TODO: this is due to bug
        config.site_import = 0;

        let status = ffi::Py_InitializeFromConfig(&config);
        assert_status(status);

        ffi::PyEval_InitThreads();
    }
}

fn init_env() {
    let guard = Python::acquire_gil();

    guard
        .python()
        .run("import cealn._init_env", None, None)
        .expect("failed to set path");
}

fn assert_status(status: ffi::PyStatus) {
    match status._type {
        ffi::PyStatusType::_PyStatus_TYPE_OK => {}
        ffi::PyStatusType::_PyStatus_TYPE_ERROR | ffi::PyStatusType::_PyStatus_TYPE_EXIT => {
            let func = unsafe {
                if !status.func.is_null() {
                    CStr::from_ptr(status.func)
                } else {
                    CStr::from_bytes_with_nul(b"[UNKNOWN]\0").unwrap()
                }
            };
            let err_msg = unsafe {
                if !status.err_msg.is_null() {
                    CStr::from_ptr(status.err_msg)
                } else {
                    CStr::from_bytes_with_nul(b"[UNKNOWN]\0").unwrap()
                }
            };

            panic!("Python initialization failed: {:?}: {:?}", func, err_msg);
        }
    }
}

fn panic_hook(panic_info: &PanicInfo) {
    if let Some(location) = panic_info.location() {
        eprint!("{}:{}: ", location.file(), location.line());
    }
    if let Some(message) = panic_info.message() {
        eprintln!("{}", message);
    }
    wasm32::unreachable();
}

fn alloc_error_hook(layout: std::alloc::Layout) {
    panic!("memory allocation of {:?} failed", layout);
}

#[cfg_attr(debug_assertions, link(name = "python3.11d", kind = "static"))]
#[cfg_attr(not(debug_assertions), link(name = "python3.11", kind = "static"))]
extern "C" {}
