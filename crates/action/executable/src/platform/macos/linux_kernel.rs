use std::sync::Arc;

use anyhow::Result;
use cealn_action_context::Context;

use crate::platform::{
    linux_process::LinuxProcess,
    vfs::{GenVfs, Vfs},
    vm::_Vm,
};

pub(super) struct VirtualKernel {
    shared: Arc<Shared>,
}

pub(super) struct Shared {
    pub vm: Arc<_Vm>,
    pub vfs: Arc<dyn GenVfs>,
}

impl VirtualKernel {
    pub fn new<C>(vm: Arc<_Vm>, vfs: Vfs<C>) -> Result<VirtualKernel>
    where
        C: Context,
    {
        let shared = Arc::new(Shared { vm, vfs: Arc::new(vfs) });
        Ok(VirtualKernel { shared })
    }

    pub fn spawn_process(&mut self, executable_path: &str) -> Result<LinuxProcess> {
        let process = LinuxProcess::new(self.shared.clone(), executable_path)?;
        Ok(process)
    }
}
