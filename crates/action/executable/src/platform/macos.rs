#[path = "macos/sys.rs"]
#[macro_use]
mod sys;

#[path = "macos/mman.rs"]
mod mman;
#[path = "macos/process.rs"]
mod process;

#[path = "macos/linux_kernel.rs"]
mod linux_kernel;
#[path = "macos/linux_process.rs"]
mod linux_process;
#[path = "macos/linux_thread.rs"]
mod linux_thread;
#[path = "macos/thread.rs"]
mod thread;
#[path = "macos/vfs.rs"]
mod vfs;
#[path = "macos/vm.rs"]
mod vm;

use std::{
    collections::BTreeMap,
    env,
    ffi::OsStr,
    io, mem,
    os::{fd::FromRawFd, unix::prelude::OsStrExt},
    path::{Path, PathBuf},
    process::Command,
    ptr,
    time::Duration,
};

use anyhow::{bail, Context as AnyhowContext, Result};
use cealn_action_context::Context;
use cealn_data::{action::{ActionOutput, ExecutePlatform, Run}, depmap::ConcreteFiletreeType};
use cealn_depset::ConcreteFiletree;
use cealn_event::{BuildEventData, EventContext};
use cealn_fs::Cachefile;
use cealn_protocol::query::{StdioLine, StdioStreamType};
use compio_core::buffer::AllowTake;
use compio_fs::{os::macos::FileExt, File, OpenOptions};
use futures::{channel::oneshot, prelude::*};
use libc::{mach_task_self, VM_FLAGS_ANYWHERE};
use memmap::MmapOptions;
use object::{
    macho::{FatHeader, MachHeader64, CPU_TYPE_ARM64},
    read::macho::{LoadCommandVariant, MachHeader, MachOFile, MachOFile32, MachOFile64},
    BigEndian, Endianness, LittleEndian, Object, ObjectSegment,
};
use serde::{Deserialize, Serialize};
use tracing::debug;

use crate::platform::{vfs::Vfs, vm::Vm};

pub async fn run<C>(context: &C, action: &Run<ConcreteFiletreeType>) -> Result<ActionOutput>
where
    C: Context,
{
    let vm = &*VM;

    match &action.platform {
        ExecutePlatform::Linux(platform) => {
            // FIXME: testing
            let mut vfs = Vfs::new(context.clone());
            vfs.mount("/", platform.execution_sysroot.clone());
            let mut kernel = vm.new_linux_virtual_kernel(vfs)?;
            let process = kernel.spawn_process("/usr/bin/true")?;
        }
        ExecutePlatform::MacOS(_) => todo!(),
    }

    todo!()
}

lazy_static::lazy_static! {
    static ref VM: Vm = Vm::new().unwrap();
}

async fn tail_file(
    mut file: File,
    mut events: EventContext,
    stream: StdioStreamType,
    mut done_rx: futures::future::Shared<oneshot::Receiver<()>>,
) -> anyhow::Result<()> {
    let mut buffer = Vec::with_capacity(128 * 1024);
    let mut shutting_down = false;
    loop {
        if buffer.capacity() == buffer.len() {
            buffer.reserve(64);
        }

        let read_count = file.read(AllowTake(&mut buffer)).await?;

        if read_count == 0 {
            if shutting_down {
                break;
            }
            match tokio::time::timeout(Duration::from_millis(100), &mut done_rx).await {
                Ok(_) => {
                    shutting_down = true;
                }
                Err(_) => continue,
            }
        }

        let consumed_len = {
            let mut buffer_remaining = &*buffer;
            while let Some(offset) = memchr::memchr(b'\n', &buffer_remaining) {
                let (head, tail) = buffer_remaining.split_at(offset);
                events.send(BuildEventData::Stdio {
                    line: StdioLine {
                        stream,
                        contents: head.to_owned(),
                    },
                });
                buffer_remaining = &tail[1..];
            }
            buffer.len() - buffer_remaining.len()
        };
        buffer.drain(..consumed_len);
        if shutting_down && !buffer.is_empty() {
            // Emit last data regardless of line content
            events.send(BuildEventData::Stdio {
                line: StdioLine {
                    stream,
                    contents: mem::replace(&mut buffer, Vec::new()),
                },
            });
        }
    }

    Ok(())
}

async fn clone_fd(cachefile: &mut Cachefile) -> anyhow::Result<std::fs::File> {
    unsafe {
        let orig_file = cachefile.ensure_open().await?;
        let new_fd = libc::fcntl(orig_file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0);
        if new_fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let file = std::fs::File::from_raw_fd(new_fd);
        Ok(file)
    }
}
