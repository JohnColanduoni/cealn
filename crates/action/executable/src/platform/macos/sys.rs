use libc::c_char;

macro_rules! hv_vm_call {
    ( $i:ident ( $( $arg:expr ),* $(,)? ) ) => {
        {
            const FUNCTION_NAME: &'static str = {
                stringify!($i)
            };
            let span = ::tracing::trace_span!(target: "hypervisor", FUNCTION_NAME);
            let _guard = span.enter();
            match $i ( $( $arg, )* ) {
                // We only use -1 (instead of < 0) for system calls that may return an address in the top half of the
                // address space
                ret if ret != HV_SUCCESS => {
                    ::tracing::error!("{} failed: {}", FUNCTION_NAME, ret);
                    ::std::result::Result::Err(::anyhow::Error::msg(format!("Hypervisor.framework error: {}", ret)))
                },
                ret => ::std::result::Result::Ok(ret),
            }
        }
    };
}

#[derive(Clone, Copy, Debug)]
#[repr(C)]
pub struct arm_thread_state64_t {
    pub x: [u64; 29],
    pub fp: u64,
    pub lr: u64,
    pub sp: u64,
    pub pc: u64,
    pub cpsr: u32,
}
