use std::{
    fmt, io,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use cealn_runtime::api::{
    types,
    types::{Errno, Rights},
    Handle, HandleRights, Result,
};

/// A file handle that simply prints to a worker
#[derive(Clone)]
pub struct PrintHandle<W>(Arc<_PrintHandle<W>>);

struct _PrintHandle<W> {
    writer: Mutex<W>,
}

impl<W: io::Write + Send + 'static> PrintHandle<W> {
    pub fn new(writer: W) -> PrintHandle<W> {
        PrintHandle(Arc::new(_PrintHandle {
            writer: Mutex::new(writer),
        }))
    }

    pub fn to_handle(&self) -> Arc<dyn Handle> {
        self.0.clone()
    }
}

#[async_trait]
impl<W: io::Write + Send + 'static> Handle for _PrintHandle<W> {
    fn file_type(&self) -> types::Filetype {
        types::Filetype::CharacterDevice
    }

    fn rights(&self) -> HandleRights {
        HandleRights::from_base(
            Rights::FD_WRITE | Rights::FD_FILESTAT_GET
            // Some programs expect to be able to run tell on stdout/stderr
            | Rights::FD_TELL,
        )
    }

    async fn write(&self, iovs: &[io::IoSlice]) -> Result<usize> {
        let mut writer = self.writer.lock().unwrap();

        writer.write_vectored(iovs).map_err(|err| match err.kind() {
            io::ErrorKind::BrokenPipe => Errno::Pipe,
            _ => Errno::Io,
        })
    }

    async fn tell(&self) -> Result<types::Filesize> {
        // Some programs expect to be able to run tell on stdout/stderr. Just return 0
        Ok(0)
    }

    async fn filestat(&self) -> Result<types::Filestat> {
        Ok(types::Filestat {
            // FIXME: should be unique
            dev: 0,
            ino: 0,
            filetype: self.file_type(),
            nlink: 1,
            size: 0,
            atim: 0,
            mtim: 0,
            ctim: 0,
        })
    }

    fn fdstat(&self) -> Result<types::Fdflags> {
        Ok(types::Fdflags::APPEND)
    }
}

impl<W> fmt::Debug for _PrintHandle<W> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("PrintHandle").finish()
    }
}
