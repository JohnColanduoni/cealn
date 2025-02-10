use std::{
    ffi::OsStr,
    mem,
    os::unix::prelude::OsStrExt,
    path::{Path, PathBuf},
    ptr,
    sync::Arc,
};

use anyhow::{bail, Context as AnyhowContext, Result};

use cealn_action_executable_macos_sys::*;
use memmap::MmapOptions;
use object::{
    macho::{FatHeader, MachHeader64, CPU_TYPE_ARM64},
    read::macho::{LoadCommandVariant, MachHeader},
    BigEndian, LittleEndian,
};
use tracing::debug;

use crate::platform::{mman::VmRegion, sys::arm_thread_state64_t, thread::Thread, vm::_Vm};
pub(super) struct Process {}

pub(super) struct _Process {
    pub vm: Arc<_Vm>,
}

impl Process {
    pub fn new(vm: Arc<_Vm>, executable_path: &Path) -> Result<Self> {
        unsafe {
            let executable_file = std::fs::File::open(&executable_path)?;

            let executable_mmap = MmapOptions::new().map(&executable_file)?;

            let offset;
            let header = match object::FileKind::parse(&*executable_mmap)? {
                object::FileKind::MachO64 => todo!(),
                object::FileKind::MachOFat64 => todo!(),
                object::FileKind::MachOFat32 => {
                    let arch_slices = FatHeader::parse_arch32(&*executable_mmap)?;
                    // FIXME: handle x86_64
                    let native_arch_slice = arch_slices
                        .iter()
                        .find(|x| x.cputype.get(BigEndian) == CPU_TYPE_ARM64)
                        .context("missing native architecture slice")?;
                    offset = native_arch_slice.offset.get(BigEndian) as usize;
                    let size = native_arch_slice.size.get(BigEndian) as usize;
                    MachHeader64::parse(&executable_mmap[..], offset as u64)?
                }
                kind => bail!("unsupported executable format {:?}", kind),
            };

            let mut exec_load_base = 0x9000000;

            let mut load_commands = header.load_commands(LittleEndian, &executable_mmap[..], offset as u64)?;
            let mut dynamic_linker = None;
            while let Some(load_command) = load_commands.next()? {
                match load_command.variant()? {
                    LoadCommandVariant::LoadDylinker(dylinker_command) => {
                        dynamic_linker = Some(load_command.string(LittleEndian, dylinker_command.name)?);
                    }
                    _ => {}
                }
            }

            let dynamic_linker = dynamic_linker.context("static executables not supported")?;
            let dynamic_linker = Path::new(OsStr::from_bytes(dynamic_linker));
            let dynamic_linker_file = std::fs::File::open(&dynamic_linker)?;
            let dynamic_linker_mmap = MmapOptions::new().map(&dynamic_linker_file)?;

            let dynamic_linker_offset;
            let dynamic_linker_header = match object::FileKind::parse(&*dynamic_linker_mmap)? {
                object::FileKind::MachOFat32 => {
                    let arch_slices = FatHeader::parse_arch32(&*dynamic_linker_mmap)?;
                    // FIXME: handle x86_64
                    let native_arch_slice = arch_slices
                        .iter()
                        .find(|x| x.cputype.get(BigEndian) == CPU_TYPE_ARM64)
                        .context("missing native architecture slice")?;
                    dynamic_linker_offset = native_arch_slice.offset.get(BigEndian) as u64;
                    let size = native_arch_slice.size.get(BigEndian) as usize;
                    MachHeader64::parse(&dynamic_linker_mmap[..], dynamic_linker_offset)?
                }
                kind => bail!("unsupported dynamic linker format {:?}", kind),
            };

            let dynamic_linker_load_base: u64 = 0x9000000;
            let mut dynamic_linker_load_commands =
                dynamic_linker_header.load_commands(LittleEndian, &dynamic_linker_mmap[..], dynamic_linker_offset)?;
            let mut dynamic_linker_vm_regions = Vec::new();
            let mut thread_state = None;
            while let Some(load_command) = dynamic_linker_load_commands.next()? {
                match load_command.variant()? {
                    LoadCommandVariant::Segment64(segment, _) => {
                        let segment_load_addr = dynamic_linker_load_base + segment.vmaddr.get(LittleEndian);
                        let file_offset = dynamic_linker_offset + segment.fileoff.get(LittleEndian);
                        let file_size = segment.filesize.get(LittleEndian) as usize;
                        let vm_size = segment.vmsize.get(LittleEndian) as usize;
                        // FIXME: disable exec as appropriate
                        let flags: u64 = (HV_MEMORY_READ | HV_MEMORY_EXEC) as u64;
                        let host_ptr = dynamic_linker_mmap.as_ptr() as usize + file_offset as usize;
                        debug!(
                            host_ptr = format_args!("{:#x}", host_ptr),
                            segment_load_addr = format_args!("{:#x}", segment_load_addr),
                            file_offset = format_args!("{:#x}", file_offset),
                            file_size = format_args!("{:#x}", file_size),
                            "loading segment"
                        );
                        let mut vm_region = VmRegion::alloc(vm_size)?;
                        vm_region.copy_from(
                            0,
                            &dynamic_linker_mmap[(file_offset as usize)..][..(file_size as usize)],
                        );
                        hv_vm_call!(hv_vm_map(vm_region.addr() as _, segment_load_addr, vm_size, flags))?;
                        dynamic_linker_vm_regions.push(vm_region);
                    }
                    LoadCommandVariant::Thread(thread_command, data) => {
                        if data.len() < mem::size_of::<arm_thread_state64_t>() {
                            bail!("thread command state too small");
                        }
                        thread_state = Some(*(data[(mem::size_of::<u64>())..].as_ptr() as *const arm_thread_state64_t));
                    }
                    _ => {}
                };
            }
            let mut thread_init_state = thread_state.context("missing thread state load command in dyld header")?;

            thread_init_state.pc += dynamic_linker_load_base;
            thread_init_state.cpsr = 0;

            let mut init_stack = VmRegion::alloc(1024 * (16 * 1024))?;
            // FIXME: don't identity map this
            let mut stack_addr = init_stack.addr() as u64;
            hv_vm_call!(hv_vm_map(
                init_stack.addr() as _,
                stack_addr,
                init_stack.size(),
                (HV_MEMORY_READ | HV_MEMORY_WRITE) as u64
            ))?;
            {
                let mut stack_builder = StackBuilder::new(&mut init_stack);
                // argc
                stack_builder.push_addr(0);
                // mach-o header pointer
                stack_builder.push_addr(exec_load_base);
                stack_builder.push_addr(exec_load_base);
                thread_init_state.sp = stack_addr + stack_builder.finish() as u64;
            }

            let shared = Arc::new(_Process { vm });

            let thread = Thread::new(shared.clone(), thread_init_state)?;

            thread.join()?;

            todo!()
        }
    }
}

struct StackBuilder<'a> {
    stack: &'a mut [u8],
}

impl<'a> StackBuilder<'a> {
    pub fn new(region: &'a mut VmRegion) -> Self {
        unsafe {
            StackBuilder {
                stack: std::slice::from_raw_parts_mut(region.addr() as *mut u8, region.size()),
            }
        }
    }

    pub fn push_addr(&mut self, addr: usize) {
        let split_at = self.stack.len() - mem::size_of::<usize>();
        self.stack[split_at..].copy_from_slice(&addr.to_ne_bytes()[..]);
        self.stack = unsafe { std::slice::from_raw_parts_mut(self.stack.as_mut_ptr(), split_at) };
    }

    pub fn finish(self) -> usize {
        self.stack.len()
    }
}
