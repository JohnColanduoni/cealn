use anyhow::Result;
use libc::mach_task_self;

use cealn_action_executable_macos_sys::*;

pub(super) struct VmRegion {
    addr: u64,
    size: u64,
}

impl Drop for VmRegion {
    fn drop(&mut self) {
        unsafe {
            mach_vm_deallocate(mach_task_self(), self.addr, self.size);
        }
    }
}

impl VmRegion {
    pub fn alloc(size: usize) -> Result<VmRegion> {
        unsafe {
            let mut addr = 0;
            mach_vm_allocate(mach_task_self(), &mut addr, size as u64, VM_FLAGS_ANYWHERE as _);
            Ok(VmRegion {
                addr,
                size: size as u64,
            })
        }
    }

    pub fn addr(&self) -> usize {
        self.addr as usize
    }

    pub fn size(&self) -> usize {
        self.size as usize
    }

    pub fn copy_from(&mut self, offset: usize, bytes: &[u8]) {
        assert!(offset + bytes.len() <= self.size as usize);
        unsafe {
            std::slice::from_raw_parts_mut((self.addr as usize + offset) as *mut u8, bytes.len()).copy_from_slice(bytes)
        }
    }
}

pub const PAGE_SIZE: usize = 16 * 1024;

pub fn round_size_to_page(size: usize) -> usize {
    (size + (PAGE_SIZE - 1)) & !(PAGE_SIZE - 1)
}
