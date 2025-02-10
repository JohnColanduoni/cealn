pub const CEALN_INTERCEPTOR_BYTES: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libcealn_interceptor.so"));

pub const INJECTION_PATH: &str = "/.cealn-inject/libcealn_interceptor.so";
