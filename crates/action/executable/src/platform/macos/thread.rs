use std::{ptr, sync::Arc, thread::JoinHandle};

use anyhow::{bail, Result};

use cealn_action_executable_macos_sys::*;
use cealn_action_executable_macos_sys as sys;
use tracing::{debug, error};

use crate::platform::{mman::VmRegion, process::_Process, sys::arm_thread_state64_t};

pub(super) struct Thread {
    host_thread: JoinHandle<Result<()>>,
}

impl Thread {
    pub fn new(process: Arc<_Process>, thread_init_state: arm_thread_state64_t) -> Result<Thread> {
        let host_thread = std::thread::Builder::new()
            .name("vm-thread".to_owned())
            .spawn(move || run(process, thread_init_state))?;
        Ok(Thread { host_thread })
    }

    pub fn join(self) -> Result<()> {
        self.host_thread.join().unwrap()
    }
}

fn run(process: Arc<_Process>, thread_init_state: arm_thread_state64_t) -> Result<()> {
    unsafe {
        let mut cpu = 0;
        let mut cpu_exit = ptr::null_mut();

        hv_vm_call!(hv_vcpu_create(&mut cpu, &mut cpu_exit, ptr::null_mut()))?;

        hv_vm_call!(hv_vcpu_set_reg(
            cpu,
            hv_reg_t_HV_REG_CPSR,
            thread_init_state.cpsr as u64
        ))?;
        hv_vm_call!(hv_vcpu_set_reg(cpu, hv_reg_t_HV_REG_PC, thread_init_state.pc))?;
        hv_vm_call!(hv_vcpu_set_sys_reg(
            cpu,
            hv_sys_reg_t_HV_SYS_REG_SP_EL0,
            thread_init_state.sp,
        ))?;

        // Setup kernel state (EL1)
        let mut kernel_stack = VmRegion::alloc(1024 * (16 * 1024))?;
        // FIXME: don't identity map this
        let mut kernel_stack_addr = kernel_stack.addr() as u64;
        hv_vm_call!(hv_vm_map(
            kernel_stack.addr() as _,
            kernel_stack_addr,
            kernel_stack.size(),
            (HV_MEMORY_READ | HV_MEMORY_WRITE) as u64
        ))?;
        hv_vm_call!(hv_vcpu_set_sys_reg(
            cpu,
            hv_sys_reg_t_HV_SYS_REG_SP_EL1,
            kernel_stack_addr + kernel_stack.size() as u64,
        ))?;
        hv_vm_call!(hv_vcpu_set_sys_reg(cpu, hv_sys_reg_t_HV_SYS_REG_SPSR_EL1, 0x1))?;
        hv_vm_call!(hv_vcpu_set_sys_reg(
            cpu,
            hv_sys_reg_t_HV_SYS_REG_VBAR_EL1,
            process.vm.vbar_el1,
        ))?;
        hv_vm_call!(hv_vcpu_set_sys_reg(
            cpu,
            hv_sys_reg_t_HV_SYS_REG_CPACR_EL1,
            // Enable floating point
            (0b11 << 20),
        ))?;
        // FIXME: other registers

        drive(cpu, cpu_exit)
    }
}

pub unsafe fn drive(cpu: hv_vcpu_t, cpu_exit: *mut hv_vcpu_exit_t) -> Result<()> {
    loop {
        hv_vm_call!(hv_vcpu_run(cpu))?;

        let cpu_exit = &*cpu_exit;
        match cpu_exit.reason {
            sys::HV_EXIT_REASON_EXCEPTION => {
                let syndrome = cpu_exit.exception.syndrome;
                let ec = (syndrome >> 26) & 0x3f;
                let faulting_addr = cpu_exit.exception.virtual_address;

                let mut pc = 0;
                hv_vm_call!(hv_vcpu_get_reg(cpu, hv_reg_t_HV_REG_PC, &mut pc))?;
                let mut lr = 0;
                hv_vm_call!(hv_vcpu_get_reg(cpu, hv_reg_t_HV_REG_LR, &mut lr))?;
                let mut fp = 0u64;
                hv_vm_call!(hv_vcpu_get_reg(cpu, hv_reg_t_HV_REG_FP, &mut fp))?;
                let mut cpsr = 0u64;
                hv_vm_call!(hv_vcpu_get_reg(cpu, hv_reg_t_HV_REG_CPSR, &mut cpsr))?;
                let el = cpsr & 0x1f;
                let mut sp = 0;
                hv_vm_call!(hv_vcpu_get_sys_reg(cpu, hv_sys_reg_t_HV_SYS_REG_SP_EL0, &mut sp))?;
                let el = debug!(
                    ec = format_args!("0b{:06b}", ec),
                    faulting_addr = format_args!("0x{:x}", faulting_addr),
                    el = format_args!("0b{:04b}", el),
                    pc = format_args!("0x{:x}", pc),
                    lr = format_args!("0x{:x}", lr),
                    sp = format_args!("0x{:x}", sp),
                    fp = format_args!("0x{:x}", fp),
                    "exception exit"
                );

                if ec == 0x16 {
                    let hvc_imm = syndrome & 0xFF;
                    debug!(hvc_imm = format_args!("0x{:x}", hvc_imm), "hvc exit");

                    match hvc_imm {
                        0x42 => {
                            // Thread exit requested
                            return Ok(());
                        }
                        0x70 => {
                            // Guest kernel panic
                            let mut message_ptr = 0u64;
                            hv_vm_call!(hv_vcpu_get_reg(cpu, hv_reg_t_HV_REG_X0, &mut message_ptr))?;
                            let mut message_len = 0u64;
                            hv_vm_call!(hv_vcpu_get_reg(cpu, hv_reg_t_HV_REG_X1, &mut message_len))?;
                            todo!("{:#x} {}", message_ptr, message_len);
                        }
                        0x71 => {
                            // Unexpected exception vector
                            let mut vector_offset = 0u64;
                            hv_vm_call!(hv_vcpu_get_reg(cpu, hv_reg_t_HV_REG_X0, &mut vector_offset))?;
                            let mut internal_syndrome = 0u64;
                            hv_vm_call!(hv_vcpu_get_sys_reg(
                                cpu,
                                hv_sys_reg_t_HV_SYS_REG_ESR_EL1,
                                &mut internal_syndrome
                            ))?;
                            let internal_ec = (internal_syndrome >> 26) & 0x3f;
                            let mut faulting_pc = 0u64;
                            hv_vm_call!(hv_vcpu_get_sys_reg(
                                cpu,
                                hv_sys_reg_t_HV_SYS_REG_ELR_EL1,
                                &mut faulting_pc
                            ))?;
                            error!(
                                vector_offset = format_args!("0x{:x}", vector_offset),
                                ec = format_args!("0b{:06b}", internal_ec),
                                faulting_pc = format_args!("0x{:x}", faulting_pc),
                                "unexpected guest kernel exception vector"
                            );
                            bail!("unexpected guest kernel exception vector");
                        }
                        hvc_imm => bail!("unknown HVC immediate 0x{:x}", hvc_imm),
                    }
                }

                // FIXME: this relies on identity mapped stack, don't do that
                while fp != 0 {
                    let prev_fp = *(fp as *const usize);
                    let prev_lr = *((fp + 8) as *const usize);
                    eprintln!("0x{:x}", prev_lr);
                    fp = prev_fp as u64;
                }

                todo!("exception");
            }
            _ => todo!(),
        }
    }
}
