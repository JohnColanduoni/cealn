use std::{
    ffi::OsStr,
    mem,
    os::unix::prelude::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::Arc,
};

use anyhow::{bail, Context as AnyhowContext, Result};

use cealn_action_context::Context;
use cealn_action_executable_macos_sys::*;
use memmap::MmapOptions;
use object::{
    macho::{FatHeader, MachHeader64, CPU_TYPE_ARM64},
    read::{
        elf::ElfFile64,
        macho::{LoadCommandVariant, MachHeader},
    },
    BigEndian, LittleEndian, Object, ObjectSegment,
};
use tracing::debug;

use crate::platform::{
    linux_kernel,
    linux_process::LinuxProcess,
    mman::{round_size_to_page, VmRegion, PAGE_SIZE},
    process::Process,
    sys::arm_thread_state64_t,
    thread::{self, Thread},
    vfs::Vfs,
};

pub(super) struct Vm {
    shared: Arc<_Vm>,
    comm_page: VmRegion,
    guest_vm_regions: Vec<VmRegion>,
}

pub(super) struct _Vm {
    pub vbar_el1: u64,
}

impl Vm {
    pub fn new() -> Result<Self> {
        unsafe {
            // Create VM
            hv_vm_call!(hv_vm_create(std::ptr::null_mut()))?;

            // Map in comm page
            let mut comm_page = VmRegion::alloc(16 * 1024)?;
            hv_vm_call!(hv_vm_map(
                comm_page.addr() as _,
                COMM_PAGE_START_ADDRESS as u64,
                comm_page.size(),
                HV_MEMORY_READ as u64
            ))?;

            // Load guest "kernel"
            let elf: ElfFile64<LittleEndian> = ElfFile64::parse(GUEST_BYTES)?;

            let mut guest_vm_regions = Vec::new();
            for segment in elf.segments() {
                let mut segment_load_addr = GUEST_KERNEL_LOAD_ADDR + segment.address();
                let (file_offset, file_size) = segment.file_range();
                let vm_size = round_size_to_page(segment.size() as usize);

                let leading_padding = if segment_load_addr % PAGE_SIZE as u64 != 0 {
                    let rem = segment_load_addr % PAGE_SIZE as u64;
                    segment_load_addr -= rem;
                    rem
                } else {
                    0
                };

                // FIXME: disable exec as appropriate
                let flags: u64 = (HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC) as u64;
                let mut vm_region = VmRegion::alloc(vm_size)?;
                debug!(
                    segment_load_addr = format_args!("{:#x}", segment_load_addr),
                    file_offset = format_args!("{:#x}", file_offset),
                    file_size = format_args!("{:#x}", file_size),
                    "loading guest kernel segment"
                );
                vm_region.copy_from(
                    leading_padding as usize,
                    &GUEST_BYTES[(file_offset as usize)..][..(file_size as usize)],
                );
                hv_vm_call!(hv_vm_map(vm_region.addr() as _, segment_load_addr, vm_size, flags))?;
                guest_vm_regions.push(vm_region);
            }

            // Run guest kernel initialization
            let mut cpu = 0;
            let mut cpu_exit = ptr::null_mut();
            hv_vm_call!(hv_vcpu_create(&mut cpu, &mut cpu_exit, ptr::null_mut()))?;
            hv_vm_call!(hv_vcpu_set_reg(
                cpu,
                hv_reg_t_HV_REG_PC,
                GUEST_KERNEL_LOAD_ADDR + elf.entry()
            ))?;
            hv_vm_call!(hv_vcpu_set_reg(cpu, hv_reg_t_HV_REG_CPSR, 0x3c4))?;
            thread::drive(cpu, cpu_exit)?;
            // Get some global state from what the kernel initialized
            let mut vbar_el1 = 0;
            hv_vm_call!(hv_vcpu_get_sys_reg(
                cpu,
                hv_sys_reg_t_HV_SYS_REG_VBAR_EL1,
                &mut vbar_el1
            ))?;
            hv_vm_call!(hv_vcpu_destroy(cpu))?;

            let shared = Arc::new(_Vm { vbar_el1 });

            Ok(Vm {
                shared,
                comm_page,
                guest_vm_regions,
            })
        }
    }

    pub fn new_linux_virtual_kernel<C>(&self, vfs: Vfs<C>) -> Result<linux_kernel::VirtualKernel>
    where
        C: Context,
    {
        let kernel = linux_kernel::VirtualKernel::new(self.shared.clone(), vfs)?;
        Ok(kernel)
    }
}

const COMM_PAGE_START_ADDRESS: usize = 0x0000000FFFFFC000;

pub const GUEST_KERNEL_LOAD_ADDR: u64 = 1u64 << 32;

#[repr(C)] // guarantee 'bytes' comes after '_align'
struct AlignedTo<Align, Bytes: ?Sized> {
    _align: [Align; 0],
    bytes: Bytes,
}

#[repr(align(16384))]
struct Align16K;

// dummy static used to create aligned data
static ALIGNED: &'static AlignedTo<Align16K, [u8]> = &AlignedTo {
    _align: [],
    bytes: *include_bytes!(concat!(env!("OUT_DIR"), "/guest")),
};

static GUEST_BYTES: &'static [u8] = &ALIGNED.bytes;

#[repr(C)]
pub struct CommPage {}
