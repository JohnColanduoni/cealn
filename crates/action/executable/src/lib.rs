#![feature(read_buf)]
#![feature(iter_advance_by)]
#![feature(let_chains)]
#![feature(core_intrinsics)]

pub mod stdio;

cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        #[path = "platform/linux.rs"]
        mod platform;
    } else if #[cfg(target_os = "macos")] {
        #[path = "platform/macos.rs"]
        mod platform;
    } else {
        compile_error!("unsupported platform");
    }
}

use std::{ffi::OsString, path::Path};

use anyhow::Result;
use futures::{
    channel::{mpsc, oneshot},
    prelude::*,
};

use cealn_action_context::Context;
use cealn_data::{
    action::{ActionOutput, Executable, ExecutePlatform, Run},
    depmap::ConcreteFiletreeType,
};
use cealn_protocol::event::BuildEvent;

pub struct Handle {
    cancel_tx: Option<oneshot::Sender<()>>,
    event_rx: mpsc::Receiver<BuildEvent>,
}

#[tracing::instrument(level = "info", err, skip(context, action), fields(executable.executable_path = action.executable.executable_path))]
pub async fn run<'a, C>(context: &'a C, action: &'a Run<ConcreteFiletreeType>) -> Result<ActionOutput>
where
    C: Context,
{
    platform::run(context, action).await
}

#[tracing::instrument(level = "info", err, skip(context, executable, platform), fields(executable.executable_path = executable.executable_path))]
pub async fn prepare_for_run<'a, C>(
    context: &'a C,
    executable: &'a Executable<ConcreteFiletreeType>,
    platform: &'a ExecutePlatform<ConcreteFiletreeType>,
    source_root: &'a Path,
) -> Result<PreparedRunGuard>
where
    C: Context,
{
    let imp = platform::prepare_for_run(context, executable, platform, source_root).await?;
    Ok(PreparedRunGuard { imp })
}

pub fn enter_prepared(parent_pid: u32, executable_path: &Path, args: &[OsString], workdir: &Path) -> Result<u32> {
    platform::enter_prepared(parent_pid, executable_path, args, workdir)
}

pub struct PreparedRunGuard {
    imp: platform::PreparedRunGuard,
}

impl PreparedRunGuard {
    #[inline]
    pub fn parent_pid(&self) -> u32 {
        self.imp.parent_pid()
    }
}
