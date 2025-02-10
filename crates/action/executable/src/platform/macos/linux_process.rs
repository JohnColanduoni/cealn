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
    elf::{PF_R, PF_W, PF_X, PT_INTERP},
    macho::{FatHeader, MachHeader64, CPU_TYPE_ARM64},
    read::{
        elf::{ElfFile64, ElfSegment64},
        macho::{LoadCommandVariant, MachHeader},
    },
    BigEndian, LittleEndian, Object, ObjectSegment,
};
use tracing::debug;

use crate::platform::{
    linux_kernel,
    linux_thread::LinuxThread,
    mman::{round_size_to_page, VmRegion, PAGE_SIZE},
    sys::arm_thread_state64_t,
    thread::Thread,
    vm::_Vm,
};
pub(super) struct LinuxProcess {
    exec_load_base: u64,
    interpreter_load_base: u64,
    vm_regions: Vec<VmRegion>,
}

pub(super) struct _LinuxProcess {
    pub kernel: Arc<linux_kernel::Shared>,
}

impl LinuxProcess {
    pub fn new(kernel: Arc<linux_kernel::Shared>, executable_path: &str) -> Result<Self> {
        unsafe {
            let mut process = LinuxProcess {
                exec_load_base: 0x8000000,
                interpreter_load_base: 0x9000000,
                vm_regions: Default::default(),
            };

            let executable_file = kernel
                .vfs
                .open_file(executable_path)?
                .with_context(|| format!("failed to find entry executable with path {:?}", executable_path))?;

            let executable_mmap = MmapOptions::new().map(&executable_file)?;

            let header = match object::FileKind::parse(&*executable_mmap)? {
                object::FileKind::Elf64 => ElfFile64::<LittleEndian>::parse(&*executable_mmap)?,
                kind => bail!("unsupported executable format {:?}", kind),
            };

            {
                let mut last_region_end = None;
                for segment in header.segments() {
                    process.load_segment(process.exec_load_base, &segment, &mut last_region_end)?;
                }
            }

            let mut interpreter = None;
            for segment in header.raw_segments() {
                if segment.p_type.get(LittleEndian) == PT_INTERP {
                    let offset: usize = segment.p_offset.get(LittleEndian).try_into()?;
                    let raw_len: usize = segment.p_filesz.get(LittleEndian).try_into()?;
                    let interpreter_bytes = &executable_mmap[offset..][..raw_len];
                    let len = interpreter_bytes
                        .iter()
                        .position(|x| *x == 0)
                        .context("missing null terminator in PT_INTERP header")?;
                    interpreter = Some(&interpreter_bytes[..len])
                }
            }

            if let Some(interpreter) = interpreter {
                let interpreter = std::str::from_utf8(interpreter).context("bad utf-8 in PT_INTERP header")?;
                debug!(interpreter, "found executable with interpreter");

                let interpreter_file = kernel
                    .vfs
                    .open_file(interpreter)?
                    .with_context(|| format!("failed to find interpreter with path {:?}", interpreter))?;

                let interpreter_mmap = MmapOptions::new().map(&interpreter_file)?;

                let interpreter_header = match object::FileKind::parse(&*interpreter_mmap)? {
                    object::FileKind::Elf64 => ElfFile64::<LittleEndian>::parse(&*interpreter_mmap)?,
                    kind => bail!("unsupported executable format {:?}", kind),
                };

                let mut last_region_end = None;
                for segment in interpreter_header.segments() {
                    process.load_segment(process.exec_load_base, &segment, &mut last_region_end)?;
                }
            } else {
                todo!()
            }

            let thread_init_state = todo!();

            let shared = Arc::new(_LinuxProcess { kernel });

            let thread = LinuxThread::new(shared.clone(), thread_init_state)?;

            thread.join()?;

            todo!()
        }
    }

    unsafe fn load_segment(
        &mut self,
        load_base: u64,
        segment: &ElfSegment64<LittleEndian>,
        last_region_end: &mut Option<u64>,
    ) -> anyhow::Result<()> {
        let mut segment_load_addr = load_base + segment.address();
        let mut segment_leading_padding = if segment_load_addr % PAGE_SIZE as u64 != 0 {
            (PAGE_SIZE as u64 - segment_load_addr % PAGE_SIZE as u64) as usize
        } else {
            0
        };
        let mut vm_size = usize::try_from(segment.size()).context("segment size too large")?;
        if let Some(last_region_end) = *last_region_end {
            if (segment_load_addr - segment_leading_padding as u64) < last_region_end {
                // FIXME: handle memory protection overlay
                segment_load_addr = last_region_end;
                segment_leading_padding = 0;
                vm_size -= (last_region_end - (segment_load_addr - segment_leading_padding as u64)) as usize;
            }
        }
        vm_size = round_size_to_page(segment_leading_padding + vm_size);
        let mut flags: u64 = 0;
        match segment.flags() {
            object::SegmentFlags::None => {}
            object::SegmentFlags::Elf { p_flags } => {
                if p_flags & PF_R != 0 {
                    flags |= HV_MEMORY_READ as u64;
                }
                if p_flags & PF_W != 0 {
                    flags |= HV_MEMORY_WRITE as u64;
                }
                if p_flags & PF_X != 0 {
                    flags |= HV_MEMORY_EXEC as u64;
                }
            }
            _ => unreachable!(),
        }
        let mut vm_region = VmRegion::alloc(vm_size)?;
        vm_region.copy_from(segment_leading_padding, segment.data()?);
        hv_vm_call!(hv_vm_map(
            vm_region.addr() as _,
            segment_load_addr - segment_leading_padding as u64,
            vm_size,
            flags
        ))?;
        self.vm_regions.push(vm_region);
        *last_region_end = Some(segment_load_addr - segment_leading_padding as u64 + vm_size as u64);

        Ok(())
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
