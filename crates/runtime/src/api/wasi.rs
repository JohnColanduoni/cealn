mod abi;
pub(crate) mod fs;

pub use self::fs::{Handle, HandleRights, InjectFdError};

use std::{
    collections::BTreeMap,
    convert::{TryFrom, TryInto},
    ffi::CString,
    fmt, io, mem,
    sync::Arc,
};

use thiserror::Error;
use tracing::{error, trace};
use wasmtime::{FuncType, Linker, Val, ValType};
use wiggle::GuestPtr;

use self::{abi::ToGuestBytes, types::Errno, wasi_snapshot_preview1::WasiSnapshotPreview1};
use super::{Api, ApiDispatch};
use crate::{api::wasi, Interpreter};
use fs::{ensure_fd_rights, ensure_handle_rights, NavigateResult};
use io::SeekFrom;

macro_rules! gen_witx {
    ($async_spec:tt) => {
        wiggle::from_witx!({
            witx: ["$CARGO_MANIFEST_DIR/witx/wasi_snapshot_preview1.witx"],
            wasmtime: false,
            async: $async_spec,
        });

        wiggle::wasmtime_integration!({
            // The wiggle code to integrate with lives here:
            target: self,
            // This must be the same witx document as used above:
            witx: ["$CARGO_MANIFEST_DIR/witx/wasi_snapshot_preview1.witx"],
            async: $async_spec
        });
    };
}

mod gen {
    gen_witx!({
        wasi_snapshot_preview1::fd_read,
        wasi_snapshot_preview1::fd_readdir,
        wasi_snapshot_preview1::fd_seek,
        wasi_snapshot_preview1::fd_tell,
        wasi_snapshot_preview1::fd_write,
        wasi_snapshot_preview1::fd_filestat_get,
        wasi_snapshot_preview1::path_filestat_get,
        wasi_snapshot_preview1::path_open,
    });
}

pub use self::gen::*;

pub struct WasiCtx {
    // TODO: Avoid this pointer chasing, probably by modifying wiggle to allow generics
    api: Box<dyn ApiDispatch>,
    fs: self::fs::SubSystem,
    envs: BTreeMap<CString, CString>,
}

pub type Result<T> = ::std::result::Result<T, Errno>;

impl WasiCtx {
    pub fn new<A: Api>(interpreter: &Interpreter, api: A) -> std::result::Result<Self, InitError> {
        let mut envs: BTreeMap<CString, CString> =
            interpreter.default_environment_variables().iter().cloned().collect();
        for (k, v) in api.envs().iter() {
            envs.insert(k.clone(), v.clone());
        }

        Ok(WasiCtx {
            fs: self::fs::SubSystem::new(
                interpreter
                    .static_filesystems()
                    .iter()
                    .cloned()
                    .map(|(k, v)| (k, v.instantiate()))
                    .chain(api.filesystems().iter().cloned())
                    .collect(),
            )?,
            api: Box::new(api),
            envs,
        })
    }

    pub(crate) fn add_to_linker(&self, linker: &mut Linker<WasiCtx>) -> anyhow::Result<()> {
        wasi::add_wasi_snapshot_preview1_to_linker(linker, move |arg| arg)?;

        linker.func_new(
            "env",
            "dup_hack",
            FuncType::new(vec![ValType::I32], vec![ValType::I32]),
            {
                move |caller, args, results| {
                    match caller.data().dup(types::Fd::from(args[0].unwrap_i32())) {
                        Ok(fd) => {
                            results[0] = Val::I32(fd.into());
                        }
                        Err(err) => results[0] = Val::I32(-(err as i32)),
                    }
                    Ok(())
                }
            },
        )?;

        Ok(())
    }

    pub fn inject_fd(
        &self,
        handle: Arc<dyn Handle>,
        fd: Option<types::Fd>,
    ) -> std::result::Result<types::Fd, InjectFdError> {
        self.fs.inject_fd(handle, fd)
    }

    pub(crate) fn dup(&self, fd: types::Fd) -> wasi::Result<types::Fd> {
        self.fs.dup(fd)
    }
}

impl types::UserErrorConversion for WasiCtx {}

// Don't show a bunch of warnings for unused variables while some options still aren't implemented
#[allow(unused_variables)]
#[wiggle::async_trait]
impl WasiSnapshotPreview1 for WasiCtx {
    fn args_get<'b>(&mut self, argv: &GuestPtr<'b, GuestPtr<'b, u8>>, argv_buf: &GuestPtr<'b, u8>) -> Result<()> {
        unimplemented!()
    }

    fn args_sizes_get(&mut self) -> Result<(types::Size, types::Size)> {
        unimplemented!()
    }

    fn environ_get<'b>(
        &mut self,
        environ: &GuestPtr<'b, GuestPtr<'b, u8>>,
        environ_buf: &GuestPtr<'b, u8>,
    ) -> Result<()> {
        let mut environ = environ.clone();
        let mut environ_buf = environ_buf.clone();

        for (k, v) in self.envs.iter() {
            trace!(code = "emit_env", key = ?TraceBuffer(k.as_bytes()), value = ?TraceBuffer(v.as_bytes()));
            let key_bytes = k.as_bytes();
            let value_bytes = v.as_bytes_with_nul();

            environ.write(environ_buf)?;

            let key_elems = key_bytes.len().try_into()?;
            let value_elems = value_bytes.len().try_into()?;
            environ_buf.as_array(key_elems).copy_from_slice(key_bytes)?;
            environ_buf = environ_buf.add(key_elems)?;
            environ_buf.as_array(1).copy_from_slice(b"=")?;
            environ_buf = environ_buf.add(1)?;
            environ_buf.as_array(value_elems).copy_from_slice(value_bytes)?;
            environ_buf = environ_buf.add(value_elems)?;

            environ = environ.add(1)?;
        }

        Ok(())
    }

    fn environ_sizes_get(&mut self) -> Result<(types::Size, types::Size)> {
        let environ_count = self.envs.len().try_into()?;
        let mut environ_size: types::Size = 0;
        for (k, v) in self.envs.iter() {
            let key_len = k.as_bytes().len().try_into()?;
            let value_len = v.as_bytes_with_nul().len().try_into()?;
            environ_size = environ_size
                .checked_add(key_len)
                .ok_or(Errno::Overflow)?
                // Equal sign
                .checked_add(1)
                .ok_or(Errno::Overflow)?
                .checked_add(value_len)
                .ok_or(Errno::Overflow)?;
        }

        Ok((environ_count, environ_size))
    }

    fn clock_res_get(&mut self, id: types::Clockid) -> Result<types::Timestamp> {
        unimplemented!()
    }

    fn clock_time_get(&mut self, id: types::Clockid, _precision: types::Timestamp) -> Result<types::Timestamp> {
        match id {
            types::Clockid::Realtime => Ok(self.api.realtime_clock()),
            types::Clockid::Monotonic => Ok(self.api.monotonic_clock()),
            id => unimplemented!("clock {:?}", id),
        }
    }

    fn fd_advise(
        &mut self,
        fd: types::Fd,
        offset: types::Filesize,
        len: types::Filesize,
        advice: types::Advice,
    ) -> Result<()> {
        unimplemented!()
    }

    fn fd_allocate(&mut self, fd: types::Fd, offset: types::Filesize, len: types::Filesize) -> Result<()> {
        unimplemented!()
    }

    fn fd_close(&mut self, fd: types::Fd) -> Result<()> {
        if !self.fs.close(fd) {
            Err(Errno::Badf)
        } else {
            Ok(())
        }
    }

    fn fd_datasync(&mut self, fd: types::Fd) -> Result<()> {
        unimplemented!()
    }

    fn fd_fdstat_get(&mut self, fd: types::Fd) -> Result<types::Fdstat> {
        let descriptor = self.fs.get_descriptor(fd).ok_or(Errno::Badf)?;
        let rights = descriptor.rights();

        Ok(types::Fdstat {
            fs_filetype: descriptor.handle().file_type(),
            fs_rights_base: rights.base,
            fs_rights_inheriting: rights.inheriting,
            fs_flags: descriptor.handle().fdstat()?,
        })
    }

    fn fd_fdstat_set_flags(&mut self, fd: types::Fd, flags: types::Fdflags) -> Result<()> {
        unimplemented!()
    }

    fn fd_fdstat_set_rights(
        &mut self,
        fd: types::Fd,
        fs_rights_base: types::Rights,
        fs_rights_inheriting: types::Rights,
    ) -> Result<()> {
        unimplemented!()
    }

    async fn fd_filestat_get(&mut self, fd: types::Fd) -> Result<types::Filestat> {
        let descriptor = self.fs.get_descriptor(fd).ok_or(Errno::Badf)?;

        let required_rights = HandleRights::from_base(types::Rights::FD_FILESTAT_GET);
        ensure_fd_rights(&descriptor, &required_rights)?;

        descriptor.handle().filestat().await
    }

    fn fd_filestat_set_size(&mut self, fd: types::Fd, size: types::Filesize) -> Result<()> {
        unimplemented!()
    }

    fn fd_filestat_set_times(
        &mut self,
        fd: types::Fd,
        atim: types::Timestamp,
        mtim: types::Timestamp,
        fst_flags: types::Fstflags,
    ) -> Result<()> {
        unimplemented!()
    }

    fn fd_pread(
        &mut self,
        fd: types::Fd,
        iovs: &types::IovecArray<'_>,
        offset: types::Filesize,
    ) -> Result<types::Size> {
        unimplemented!()
    }

    fn fd_prestat_get(&mut self, fd: types::Fd) -> Result<types::Prestat> {
        let descriptor = self.fs.get_descriptor(fd).ok_or(Errno::Badf)?;

        let preopen_path = descriptor.preopen_path().ok_or(Errno::Notsup)?;

        if descriptor.file_type() != types::Filetype::Directory {
            return Err(Errno::Notdir);
        }

        Ok(types::Prestat::Dir(types::PrestatDir {
            pr_name_len: preopen_path.len().try_into()?,
        }))
    }

    fn fd_prestat_dir_name(&mut self, fd: types::Fd, path: &GuestPtr<u8>, path_len: types::Size) -> Result<()> {
        let descriptor = self.fs.get_descriptor(fd).ok_or(Errno::Badf)?;

        let preopen_path = descriptor.preopen_path().ok_or(Errno::Notsup)?;

        if descriptor.file_type() != types::Filetype::Directory {
            return Err(Errno::Notdir);
        }

        let actual_path_len = preopen_path.len().try_into()?;

        if actual_path_len > path_len {
            return Err(Errno::Nametoolong);
        }

        trace!(code = "prestat_path", path = preopen_path);

        path.as_array(actual_path_len)
            .copy_from_slice(preopen_path.as_bytes())?;

        Ok(())
    }

    fn fd_pwrite(
        &mut self,
        fd: types::Fd,
        ciovs: &types::CiovecArray<'_>,
        offset: types::Filesize,
    ) -> Result<types::Size> {
        unimplemented!()
    }

    async fn fd_read<'a>(&mut self, fd: types::Fd, iovs: &types::IovecArray<'a>) -> Result<types::Size> {
        // TODO: use small vecs here
        let mut guest_slices = Vec::new();
        for iov_ptr in iovs.iter() {
            let iov_ptr = iov_ptr?;
            let iov = iov_ptr.read()?;
            guest_slices.push(iov.buf.as_array(iov.buf_len).as_slice_mut()?.ok_or(Errno::Inval)?);
        }

        let descriptor = self.fs.get_descriptor(fd).ok_or(Errno::Badf)?;

        ensure_fd_rights(&descriptor, &HandleRights::from_base(types::Rights::FD_READ))?;

        let mut native_iov: Vec<_> = guest_slices.iter_mut().map(|s| io::IoSliceMut::new(&mut *s)).collect();
        let handle = descriptor.handle().clone();
        let read_bytes = descriptor.handle().read(&mut native_iov).await?;

        Ok(read_bytes.try_into()?)
    }

    async fn fd_readdir<'a>(
        &mut self,
        fd: types::Fd,
        buf_start: &GuestPtr<'a, u8>,
        buf_len: types::Size,
        cookie: types::Dircookie,
    ) -> Result<types::Size> {
        let descriptor = self.fs.get_descriptor(fd).ok_or(Errno::Badf)?;

        if !descriptor.check_rights(&HandleRights::from_base(types::Rights::FD_READDIR)) {
            error!("access check failed for readdir");
            return Err(Errno::Notcapable);
        }

        let mut written_bytes = 0;
        let mut buf = buf_start.clone();
        let handle = descriptor.handle_clone();
        mem::drop(descriptor);
        for pair in handle.readdir(cookie).await? {
            let (dirent, name) = pair?;

            let dirent_raw = dirent.to_guest_bytes();
            let dirent_len: types::Size = dirent_raw.len().try_into()?;
            let name_raw = name.as_bytes();
            let name_len = name_raw.len().try_into()?;

            if (buf_len - written_bytes) < dirent_len {
                let rem_bytes = buf_len - written_bytes;
                buf.as_array(rem_bytes)
                    .copy_from_slice(&dirent_raw[..usize::try_from(rem_bytes).map_err(|_| Errno::Overflow)?])?;

                written_bytes += rem_bytes;

                break;
            } else {
                buf.as_array(dirent_len).copy_from_slice(&dirent_raw)?;
                buf = buf.add(dirent_len)?;

                written_bytes += dirent_len;
            }

            if (buf_len - written_bytes) < name_len {
                let rem_bytes = buf_len - written_bytes;
                buf.as_array(rem_bytes)
                    .copy_from_slice(&name_raw[..usize::try_from(rem_bytes).map_err(|_| Errno::Overflow)?])?;

                written_bytes += rem_bytes;

                break;
            } else {
                buf.as_array(name_len).copy_from_slice(name_raw)?;
                buf = buf.add(name_len)?;
                written_bytes += name_len;
            }
        }

        Ok(written_bytes)
    }

    fn fd_renumber(&mut self, from: types::Fd, to: types::Fd) -> Result<()> {
        unimplemented!()
    }

    async fn fd_seek(
        &mut self,
        fd: types::Fd,
        offset: types::Filedelta,
        whence: types::Whence,
    ) -> Result<types::Filesize> {
        let descriptor = self.fs.get_descriptor(fd).ok_or(Errno::Badf)?;

        let required_rights = if whence == types::Whence::Cur {
            if offset == 0 {
                HandleRights::from_base(types::Rights::FD_TELL)
            } else {
                HandleRights::from_base(types::Rights::FD_SEEK | types::Rights::FD_TELL)
            }
        } else {
            HandleRights::from_base(types::Rights::FD_SEEK)
        };
        ensure_fd_rights(&descriptor, &required_rights)?;

        let pos = match whence {
            types::Whence::Set => SeekFrom::Start(u64::try_from(offset).map_err(|_| Errno::Inval)?),
            types::Whence::End => SeekFrom::End(offset),
            types::Whence::Cur => SeekFrom::Current(offset),
        };

        // Linux-oriented programs often use lseek(fd, 0, SEEK_CUR) to get file position. In addition to the following
        // the WASI spec's use of `types::Rights::FD_TELL` in those situations, we make things easier on our filesystem
        // implementations by mapping this to a tell, rather than seek.
        let handle = descriptor.handle_clone();
        mem::drop(descriptor);
        if let SeekFrom::Current(0) = pos {
            handle.tell().await
        } else {
            handle.seek(pos).await
        }
    }

    fn fd_sync(&mut self, fd: types::Fd) -> Result<()> {
        unimplemented!()
    }

    async fn fd_tell(&mut self, fd: types::Fd) -> Result<types::Filesize> {
        let descriptor = self.fs.get_descriptor(fd).ok_or(Errno::Badf)?;

        let required_rights = HandleRights::from_base(types::Rights::FD_TELL);
        ensure_fd_rights(&descriptor, &required_rights)?;

        let handle = descriptor.handle_clone();
        mem::drop(descriptor);

        handle.tell().await
    }

    async fn fd_write<'a>(&mut self, fd: types::Fd, ciovs: &types::CiovecArray<'a>) -> Result<types::Size> {
        // TODO: use small vecs here
        let mut guest_slices = Vec::new();
        for iov_ptr in ciovs.iter() {
            let iov_ptr = iov_ptr?;
            let iov = iov_ptr.read()?;
            let slice = iov.buf.as_array(iov.buf_len).as_slice()?.ok_or(Errno::Inval)?;

            trace!(code = "write_slice", data = ?TraceBuffer(&*slice));

            guest_slices.push(slice);
        }

        let descriptor = self.fs.get_descriptor(fd).ok_or(Errno::Badf)?;

        ensure_fd_rights(&descriptor, &HandleRights::from_base(types::Rights::FD_WRITE))?;

        let handle = descriptor.handle_clone();
        mem::drop(descriptor);

        let native_iov: Vec<_> = guest_slices.iter_mut().map(|s| io::IoSlice::new(&*s)).collect();
        let write_bytes = handle.write(&native_iov).await?;

        Ok(write_bytes.try_into()?)
    }

    fn path_create_directory(&mut self, dirfd: types::Fd, path: &GuestPtr<'_, str>) -> Result<()> {
        let path = path.as_str()?.ok_or(Errno::Inval)?;

        trace!(code = "decoded_path", path = &*path);

        let descriptor = self.fs.get_descriptor(dirfd).ok_or(Errno::Badf)?;

        ensure_fd_rights(
            &descriptor,
            &HandleRights::from_base(types::Rights::PATH_CREATE_DIRECTORY),
        )?;

        unimplemented!()
    }

    async fn path_filestat_get<'a>(
        &mut self,
        dirfd: types::Fd,
        flags: types::Lookupflags,
        path: &GuestPtr<'a, str>,
    ) -> Result<types::Filestat> {
        let path = path.as_str()?.ok_or(Errno::Inval)?;

        trace!(code = "decoded_path", path = &*path);

        let descriptor = self.fs.get_descriptor(dirfd).ok_or(Errno::Badf)?;
        if descriptor.file_type() != types::Filetype::Directory {
            return Err(Errno::Notdir);
        }

        // FIXME: symlink follow permissions
        let required_rights = HandleRights::from_base(types::Rights::PATH_FILESTAT_GET);

        match descriptor.navigate(&path, &required_rights, flags).await? {
            NavigateResult::Parented {
                parent_handle,
                filename,
            } => {
                ensure_handle_rights(&*parent_handle, &required_rights)?;

                parent_handle.filestat_child(&filename).await
            }
            NavigateResult::Unparented { directory_handle } => {
                ensure_handle_rights(&*directory_handle, &required_rights)?;

                directory_handle.filestat().await
            }
        }
    }

    fn path_filestat_set_times(
        &mut self,
        dirfd: types::Fd,
        flags: types::Lookupflags,
        path: &GuestPtr<'_, str>,
        atim: types::Timestamp,
        mtim: types::Timestamp,
        fst_flags: types::Fstflags,
    ) -> Result<()> {
        unimplemented!()
    }

    fn path_link(
        &mut self,
        old_fd: types::Fd,
        old_flags: types::Lookupflags,
        old_path: &GuestPtr<'_, str>,
        new_fd: types::Fd,
        new_path: &GuestPtr<'_, str>,
    ) -> Result<()> {
        unimplemented!()
    }

    async fn path_open<'a>(
        &mut self,
        dirfd: types::Fd,
        dirflags: types::Lookupflags,
        path: &GuestPtr<'a, str>,
        oflags: types::Oflags,
        fs_rights_base: types::Rights,
        fs_rights_inheriting: types::Rights,
        fdflags: types::Fdflags,
    ) -> Result<types::Fd> {
        // Calculate needed rights
        // TODO: check this, this was cargo culted from wasmtime's wasi-common
        let mut needed_base = types::Rights::PATH_OPEN;
        let mut needed_inheriting = fs_rights_base | fs_rights_inheriting;

        if oflags.contains(types::Oflags::CREAT) {
            needed_base |= types::Rights::PATH_CREATE_FILE;
        }
        if oflags.contains(types::Oflags::TRUNC) {
            needed_base |= types::Rights::PATH_FILESTAT_SET_SIZE;
        }

        if fdflags.contains(types::Fdflags::DSYNC) {
            needed_inheriting |= types::Rights::FD_DATASYNC;
        }
        if fdflags.contains(types::Fdflags::SYNC) | fdflags.contains(types::Fdflags::RSYNC) {
            needed_inheriting |= types::Rights::FD_SYNC;
        }

        let parent_rights = HandleRights::new(needed_base, needed_inheriting);
        // FIXME: I think we need to handle DSYNC/SYNC/RSYNC here too
        let opened_rights = HandleRights::new(fs_rights_base, fs_rights_inheriting);

        let path = path.as_str()?.ok_or(Errno::Inval)?;
        trace!(code = "decoded_path", path = &*path);
        let nav_result = {
            // Ensure we don't borrow descriptor table while we add descriptor
            let descriptor = self.fs.get_descriptor(dirfd).ok_or(Errno::Badf)?;
            descriptor.navigate(&path, &parent_rights, dirflags).await?
        };

        match nav_result {
            NavigateResult::Parented {
                parent_handle,
                filename,
            } => {
                ensure_handle_rights(&*parent_handle, &parent_rights)?;

                let handle = parent_handle
                    .openat_child(
                        &filename,
                        // FIXME: don't think these are broad enough
                        fs_rights_base.contains(types::Rights::FD_READ),
                        fs_rights_base.contains(types::Rights::FD_WRITE),
                        oflags,
                        fdflags,
                    )
                    .await?;

                // Filter out rights that don't apply to the target file type
                let opened_rights = opened_rights.filter_for_type(handle.file_type());

                ensure_handle_rights(&*handle, &opened_rights)?;

                self.fs.new_descriptor(handle, Some(&opened_rights))
            }
            // FIXME: not sure permission check is resolved in this case
            NavigateResult::Unparented { directory_handle } => {
                // Filter out rights that don't apply to the target file type
                let opened_rights = opened_rights.filter_for_type(directory_handle.file_type());

                ensure_handle_rights(&*directory_handle, &opened_rights)?;

                self.fs.new_descriptor(directory_handle, Some(&opened_rights))
            }
        }
    }

    fn path_readlink(
        &mut self,
        dirfd: types::Fd,
        path: &GuestPtr<'_, str>,
        buf: &GuestPtr<u8>,
        buf_len: types::Size,
    ) -> Result<types::Size> {
        unimplemented!()
    }

    fn path_remove_directory(&mut self, dirfd: types::Fd, path: &GuestPtr<'_, str>) -> Result<()> {
        unimplemented!()
    }

    fn path_rename(
        &mut self,
        old_fd: types::Fd,
        old_path: &GuestPtr<'_, str>,
        new_fd: types::Fd,
        new_path: &GuestPtr<'_, str>,
    ) -> Result<()> {
        unimplemented!()
    }

    fn path_symlink(
        &mut self,
        old_path: &GuestPtr<'_, str>,
        dirfd: types::Fd,
        new_path: &GuestPtr<'_, str>,
    ) -> Result<()> {
        unimplemented!()
    }

    fn path_unlink_file(&mut self, dirfd: types::Fd, path: &GuestPtr<'_, str>) -> Result<()> {
        unimplemented!()
    }

    fn poll_oneoff(
        &mut self,
        in_: &GuestPtr<types::Subscription>,
        out: &GuestPtr<types::Event>,
        nsubscriptions: types::Size,
    ) -> Result<types::Size> {
        unimplemented!()
    }

    fn proc_exit(&mut self, _rval: types::Exitcode) -> anyhow::Error {
        unimplemented!()
    }

    fn proc_raise(&mut self, _sig: types::Signal) -> Result<()> {
        unimplemented!()
    }

    fn sched_yield(&mut self) -> Result<()> {
        unimplemented!()
    }

    fn random_get(&mut self, buf: &GuestPtr<u8>, buf_len: types::Size) -> Result<()> {
        // There's not a good way to do this while maintaining a safe deterministic environment, so we entirely disable
        // this functionality.
        Err(Errno::Nosys)
    }

    fn sock_accept(&mut self, _fd: types::Fd, _flags: types::Fdflags) -> Result<types::Fd> {
        unimplemented!()
    }

    fn sock_recv(
        &mut self,
        _fd: types::Fd,
        _ri_data: &types::IovecArray<'_>,
        _ri_flags: types::Riflags,
    ) -> Result<(types::Size, types::Roflags)> {
        unimplemented!()
    }

    fn sock_send(
        &mut self,
        _fd: types::Fd,
        _si_data: &types::CiovecArray<'_>,
        _si_flags: types::Siflags,
    ) -> Result<types::Size> {
        unimplemented!()
    }

    fn sock_shutdown(&mut self, _fd: types::Fd, _how: types::Sdflags) -> Result<()> {
        unimplemented!()
    }
}

impl wiggle::GuestErrorType for Errno {
    fn success() -> Self {
        Self::Success
    }
}

impl From<std::num::TryFromIntError> for Errno {
    fn from(_err: std::num::TryFromIntError) -> Self {
        Self::Overflow
    }
}

impl From<wiggle::GuestError> for Errno {
    fn from(err: wiggle::GuestError) -> Self {
        trace!(code = "mapping_wiggle_error", err = ?err);
        // TODO: map errors
        Errno::Inval
    }
}

#[derive(Error, Debug)]
pub enum InitError {
    #[error("failed to initialize filesystem: {0}")]
    Filesystem(#[from] self::fs::InitError),
}

/// Wrapper for printing out raw buffers that may or may not contain text
struct TraceBuffer<'a>(&'a [u8]);

impl fmt::Debug for TraceBuffer<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(r#"b""#)?;
        for b in self.0.iter().cloned() {
            fmt::Display::fmt(&std::ascii::escape_default(b), f)?;
        }
        f.write_str(r#"""#)?;
        Ok(())
    }
}
