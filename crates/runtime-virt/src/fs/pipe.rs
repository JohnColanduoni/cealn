use std::{
    cell::Cell,
    cmp,
    collections::VecDeque,
    fmt, io,
    sync::{Arc, Mutex},
    task::Waker,
};

use async_trait::async_trait;
pub use cealn_runtime::api::{types, types::Errno, Handle, HandleRights, Result};

#[derive(Clone)]
pub struct PipeFs(Arc<_PipeFs>);

struct _PipeFs {
    next_inode: Cell<u64>,
}

#[derive(Clone)]
pub struct Pipe(Arc<_Pipe>);

struct _Pipe {
    inode: u64,
    write: Option<Mutex<Buffer>>,
    read: Option<Mutex<Buffer>>,
}

struct Buffer {
    data: VecDeque<u8>,
    read_waiters: Vec<Waker>,
}

struct PipeHandle {
    shared: Arc<_Pipe>,
}

impl PipeFs {
    pub fn new() -> Self {
        PipeFs(Arc::new(_PipeFs {
            next_inode: Cell::new(0),
        }))
    }

    pub fn new_pipe(&self, read: bool, write: bool) -> Pipe {
        let inode = self.0.next_inode.get();
        self.0.next_inode.set(inode.checked_add(1).unwrap());
        Pipe(Arc::new(_Pipe {
            inode,
            write: if write { Some(Mutex::new(Buffer::new())) } else { None },
            read: if read { Some(Mutex::new(Buffer::new())) } else { None },
        }))
    }
}

impl Pipe {
    pub fn to_handle(&self) -> Arc<dyn Handle> {
        Arc::new(PipeHandle { shared: self.0.clone() })
    }
}

impl io::Read for Pipe {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut buffer = self
            .0
            .write
            .as_ref()
            .expect("pipe doesn't allow reading by the host")
            .lock()
            .unwrap();

        let bytes_read = buffer.read_nonblock(&mut [io::IoSliceMut::new(buf)]);

        if bytes_read == 0 {
            Err(io::ErrorKind::WouldBlock.into())
        } else {
            Ok(bytes_read)
        }
    }
}

impl io::Write for Pipe {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut buffer = self
            .0
            .read
            .as_ref()
            .expect("pipe doesn't allow writing by the host")
            .lock()
            .unwrap();

        Ok(buffer.write(&[io::IoSlice::new(buf)]))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Buffer {
    fn new() -> Buffer {
        Buffer {
            data: Default::default(),
            read_waiters: Default::default(),
        }
    }

    fn read_nonblock(&mut self, iovs: &mut [io::IoSliceMut]) -> usize {
        let (a, b) = self.data.as_slices();

        let mut current_slice = a;
        let next_slice = Some(b);

        let mut bytes_read = 0usize;
        'vecloop: for vec in iovs.iter_mut() {
            let mut vec_rem = &mut vec[..];

            loop {
                let op_bytes = cmp::min(vec_rem.len(), current_slice.len());

                if op_bytes == 0 && !vec_rem.is_empty() {
                    break 'vecloop;
                }

                vec_rem[..op_bytes].copy_from_slice(&current_slice[..op_bytes]);

                current_slice = &current_slice[op_bytes..];
                vec_rem = &mut vec_rem[op_bytes..];
                bytes_read += op_bytes;

                if current_slice.is_empty() {
                    match next_slice {
                        Some(slice) => {
                            current_slice = slice;
                            if vec_rem.is_empty() {
                                continue 'vecloop;
                            }
                        }
                        None => break 'vecloop,
                    }
                } else {
                    continue 'vecloop;
                }
            }
        }

        bytes_read
    }

    fn write(&mut self, iovs: &[io::IoSlice]) -> usize {
        let mut bytes_to_write = 0usize;
        for vec in iovs.iter() {
            bytes_to_write += vec.len();
        }
        self.data.reserve(bytes_to_write);

        for vec in iovs.iter() {
            self.data.extend(vec.iter().cloned());
        }

        for reader in self.read_waiters.drain(..) {
            reader.wake();
        }

        bytes_to_write
    }
}

#[async_trait]
impl Handle for PipeHandle {
    fn file_type(&self) -> types::Filetype {
        types::Filetype::CharacterDevice
    }

    fn rights(&self) -> HandleRights {
        let mut rights = types::Rights::FD_FILESTAT_GET;
        if self.shared.read.is_some() {
            rights |= types::Rights::FD_READ;
        }
        if self.shared.write.is_some() {
            rights |= types::Rights::FD_WRITE;
        }
        HandleRights::from_base(rights)
    }

    async fn read(&self, _iovs: &mut [io::IoSliceMut]) -> Result<usize> {
        unimplemented!()
    }

    async fn write(&self, iovs: &[io::IoSlice]) -> Result<usize> {
        let mut buffer = self.shared.write.as_ref().ok_or(Errno::Notcapable)?.lock().unwrap();
        Ok(buffer.write(iovs))
    }

    async fn filestat(&self) -> Result<types::Filestat> {
        Ok(types::Filestat {
            // FIXME: should be unique
            dev: 0,
            ino: self.shared.inode,
            filetype: types::Filetype::CharacterDevice,
            nlink: 1,
            size: 0,
            atim: 0,
            mtim: 0,
            ctim: 0,
        })
    }
}

impl fmt::Debug for PipeHandle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("PipeHandle").field("inode", &self.shared.inode).finish()
    }
}
