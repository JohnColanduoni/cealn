use libc::{c_char, size_t};
use crate::wchar_t;

#[cfg(any(Py_3_5, not(Py_LIMITED_API)))]
#[cfg_attr(windows, link(name = "pythonXY"))]
extern "C" {
    pub fn Py_DecodeLocale(arg: *const c_char, size: *mut size_t) -> *const wchar_t;
    pub fn Py_EncodeLocale(text: *const wchar_t, error_pos: *mut size_t) -> *const c_char;
}
