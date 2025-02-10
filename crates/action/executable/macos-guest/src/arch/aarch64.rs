use core::arch::{asm, naked_asm};

#[no_mangle]
#[naked]
pub unsafe extern "C" fn _start() {
    naked_asm!(
        // Setup stack
        "mov x9, #0x1",
        "msr spsel, x9",
        "adrp x9, {bootstrap_stack}",
        "add x9, x9, :lo12:{bootstrap_stack}",
        "add x9, x9, {stack_size}",
        "mov sp, x9",

        // Setup EVT
        "adrp x9, {exception_vector_table}",
        "add x9, x9, :lo12:{exception_vector_table}",
        "msr VBAR_EL1, x9",

        // Call main
        "b {main}",
        main = sym crate::main,
        bootstrap_stack = sym crate::BOOTSTRAP_KERNEL_STACK,
        stack_size = const crate::KERNEL_STACK_SIZE,
        exception_vector_table = sym exception_vector_table,
    );
}

pub fn main_hypervisor_exit() -> ! {
    unsafe {
        asm!("hvc #0x42", options(noreturn));
    }
}

pub fn panic_exit(message: &[u8]) -> ! {
    unsafe {
        asm!(
            "mov x0, {ptr}",
            "mov x1, {len}",
            "hvc #0x70",
            ptr = in(reg) message.as_ptr(),
            len = in(reg) message.len(),
            options(noreturn)
        )
    }
}

#[naked]
pub unsafe extern "C" fn exception_vector_table() {
    naked_asm!(
        ".balign 0x800",
        // Current EL, SP0, Synchronous
        "mov x0, #0x0",
        "b {unexpected_vector}",
        // Current EL, SP0, IRQ
        ".balign 0x80",
        "mov x0, #0x80",
        "b {unexpected_vector}",
        // Current EL, SP0, FIQ
        ".balign 0x80",
        "mov x0, #0x100",
        "b {unexpected_vector}",
        // Current EL, SP0, SError
        ".balign 0x80",
        "mov x0, #0x180",
        "b {unexpected_vector}",
        // Current EL, SPx, Synchronous
        ".balign 0x80",
        "mov x0, #0x200",
        "b {unexpected_vector}",
        // Current EL, SPx, IRQ
        ".balign 0x80",
        "mov x0, #0x280",
        "b {unexpected_vector}",
        // Current EL, SPx, FIQ
        ".balign 0x80",
        "mov x0, #0x300",
        "b {unexpected_vector}",
        // Current EL, SPx, SError
        ".balign 0x80",
        "mov x0, #0x380",
        "b {unexpected_vector}",
        // Lower EL, Synchronous
        ".balign 0x80",
        "mov x0, #0x400",
        "b {lower_el_sync_entry}",
        // Lower EL, IRQ
        ".balign 0x80",
        "mov x0, #0x480",
        "b {unexpected_vector}",
        // Lower EL, FIQ
        ".balign 0x80",
        "mov x0, #0x400",
        "b {unexpected_vector}",
        // Lower EL, SError
        ".balign 0x80",
        "mov x0, #0x480",
        "b {unexpected_vector}",
        unexpected_vector = sym unexpected_vector,
        lower_el_sync_entry = sym lower_el_sync_entry,
    )
}

#[naked]
unsafe extern "C" fn lower_el_sync_entry() -> ! {
    naked_asm!(
        "stp x29, x30, [sp, #-0x10]!",
        "stp x27, x28, [sp, #-0x10]!",
        "stp x25, x26, [sp, #-0x10]!",
        "stp x23, x24, [sp, #-0x10]!",
        "stp x21, x22, [sp, #-0x10]!",
        "stp x19, x20, [sp, #-0x10]!",
        "stp x17, x18, [sp, #-0x10]!",
        "stp x15, x16, [sp, #-0x10]!",
        "stp x13, x14, [sp, #-0x10]!",
        "stp x11, x12, [sp, #-0x10]!",
        "stp x9, x10, [sp, #-0x10]!",
        "stp x7, x8, [sp, #-0x10]!",
        "stp x5, x6, [sp, #-0x10]!",
        "stp x3, x4, [sp, #-0x10]!",
        "stp x1, x2, [sp, #-0x10]!",
        "mrs x21, spsr_el1",
        "mrs x22, elr_el1",
        // Calculate original stack pointer
        "add x23, sp, 0xF0",
        "stp x21, x0, [sp, #-0x10]!",
        "stp x23, x22, [sp, #-0x10]!",
        // Provide pointer to ExceptionRegs as first argument
        "mov x0, sp",
        "bl {lower_el_sync}",
        "ldp x23, x22, [sp], #0x10",
        "ldp x21, x0, [sp], #0x10",
        "msr spsr_el1, x21",
        "msr elr_el1, x22",
        "ldp x1, x2, [sp], #0x10",
        "ldp x3, x4, [sp], #0x10",
        "ldp x5, x6, [sp], #0x10",
        "ldp x7, x8, [sp], #0x10",
        "ldp x9, x10, [sp], #0x10",
        "ldp x11, x12, [sp], #0x10",
        "ldp x13, x14, [sp], #0x10",
        "ldp x15, x16, [sp], #0x10",
        "ldp x17, x18, [sp], #0x10",
        "ldp x19, x20, [sp], #0x10",
        "ldp x21, x22, [sp], #0x10",
        "ldp x23, x24, [sp], #0x10",
        "ldp x25, x26, [sp], #0x10",
        "ldp x27, x28, [sp], #0x10",
        "ldp x29, x30, [sp], #0x10",
        "eret",
        lower_el_sync = sym lower_el_sync,
    );
}

#[derive(Clone, Copy, Default, Debug)]
#[repr(C, packed)]
pub struct ExceptionRegs {
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
    pub gp: [u64; 31],
}

unsafe extern "C" fn lower_el_sync(regs: *mut ExceptionRegs) {
    todo!()
}

#[naked]
unsafe extern "C" fn unexpected_vector() -> ! {
    naked_asm!("hvc #0x71")
}
