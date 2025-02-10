use std::{
    collections::{btree_map, BTreeMap},
    io::{self, Seek, SeekFrom},
    mem,
    panic::{self, AssertUnwindSafe},
    path::Path,
    process::Command,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex,
    },
    thread::JoinHandle,
    time::Duration,
};

use anyhow::{bail, Result, Context as _};
use cealn_action_context::Context as ActionContext;
use cealn_data::{
    file_entry::{FileEntry, FileEntryRef, FileHash},
    label::{LabelPath, LabelPathBuf, NormalizedDescending},
};
use cealn_depset::ConcreteFiletree;
use dashmap::DashMap;
use fuse_backend_rs::{
    abi::fuse_abi::{InHeader, Opcode, OutHeader},
    api::{
        filesystem::{
            Context, DirEntry, Entry, FileSystem, FsOptions, GetxattrReply, ListxattrReply, OpenOptions, ZeroCopyWriter,
        },
        server::Server,
    },
    transport::{FuseChannel, FuseDevWriter, FuseSession, Reader, Writer},
};
use futures::{future::RemoteHandle, Future};
use hashbrown::HashMap;
use libc::c_int;
use tracing::{debug_span, error, trace, trace_span};

pub const CACHE_MOUNT_PATH: &str = "/.cealn-cache";

pub struct Mount {
    session: Arc<Mutex<FuseSession>>,
    runners: Vec<JoinHandle<Result<()>>>,
    unmounted: bool,
}

impl Drop for Mount {
    fn drop(&mut self) {
        if !self.unmounted {
            let mut session = self.session.lock().unwrap();
            let _ = session.wake();
            let _ = session.umount();
        }
    }
}

#[tracing::instrument(level = "debug", err, skip(context, depmap))]
pub fn mount_depmap<C: ActionContext>(context: &C, depmap: ConcreteFiletree, mount_dir: &Path) -> Result<Mount> {
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };
    let time = 1683413598;

    let mut inodes = HashMap::new();
    let mut entries = BTreeMap::new();
    {
        let span = debug_span!("build_inodes", inode_count = tracing::field::Empty);
        let _guard = span.enter();
        {
            let mut stat: libc::stat64 = unsafe { mem::zeroed() };
            stat.st_ino = 1;
            stat.st_dev = FAKE_DEV_ID;
            stat.st_nlink = 2;
            stat.st_mode = libc::S_IFDIR | 0o777;
            stat.st_uid = uid;
            stat.st_gid = gid;
            stat.st_size = 4096;
            stat.st_blksize = 4096;
            stat.st_blocks = 8;
            stat.st_mtime = time;
            stat.st_atime = time;
            stat.st_ctime = time;
            inodes.insert(
                1,
                Inode {
                    stat,
                    subpath: LabelPath::new("")
                        .unwrap()
                        .normalize_require_descending()
                        .unwrap()
                        .into_owned(),
                    content: None,
                    executable: false,
                    target: None,
                },
            );
        }
        let mut next_ino = 2;

        for entry in depmap.iter() {
            let (k, v) = entry?;

            let mut current_parent = k.as_ref().parent();
            while let Some(parent) = current_parent {
                let parent_inode = entries
                    .get(parent.into_inner())
                    .and_then(|inode_nr| inodes.get(inode_nr));
                if parent_inode.map(|x| x.stat.st_mode & libc::S_IFMT == libc::S_IFDIR) != Some(true) {
                    // Implicit directory
                    let mut stat: libc::stat64 = unsafe { mem::zeroed() };
                    stat.st_ino = next_ino;
                    stat.st_dev = FAKE_DEV_ID;
                    stat.st_nlink = 2;
                    stat.st_mode = libc::S_IFDIR | 0o777;
                    stat.st_uid = uid;
                    stat.st_gid = gid;
                    stat.st_size = 4096;
                    stat.st_blksize = 4096;
                    stat.st_blocks = 8;
                    stat.st_mtime = time;
                    stat.st_atime = time;
                    stat.st_ctime = time;
                    inodes.insert(
                        next_ino,
                        Inode {
                            stat,
                            subpath: parent.to_owned(),
                            content: None,
                            executable: false,
                            target: None,
                        },
                    );
                    entries.insert(parent.to_owned(), next_ino);
                    next_ino += 1;
                    current_parent = parent.parent();
                } else {
                    // Found existing parent, so we know all its parents exist too
                    break;
                }
            }

            let existing_entry = entries.entry(k.into_owned());

            match v {
                FileEntryRef::Regular {
                    content_hash,
                    executable,
                } => {
                    // Skip creating duplicate entries (these are common when building depmaps from things like ninja files)
                    if let btree_map::Entry::Occupied(existing_entry) = &existing_entry {
                        let inode = &inodes[existing_entry.get()];
                        if inode.content.as_ref().map(|x| x.as_ref()) == Some(content_hash)
                            && inode.executable == executable
                        {
                            continue;
                        }
                    }

                    let mut stat: libc::stat64 = unsafe { mem::zeroed() };
                    stat.st_ino = next_ino;
                    stat.st_dev = FAKE_DEV_ID;
                    stat.st_nlink = 1;
                    stat.st_mode = libc::S_IFREG | if executable { 0o555 } else { 0o444 };
                    stat.st_uid = uid;
                    stat.st_gid = gid;
                    stat.st_size = 0;
                    stat.st_blksize = 4096;
                    stat.st_blocks = 0;
                    stat.st_mtime = time;
                    stat.st_atime = time;
                    stat.st_ctime = time;
                    inodes.insert(
                        next_ino,
                        Inode {
                            stat,
                            subpath: existing_entry.key().to_owned(),
                            content: Some(content_hash.to_owned()),
                            executable,
                            target: None,
                        },
                    );
                }
                FileEntryRef::Symlink(target) => {
                    // Skip creating duplicate entries (these are common when building depmaps from things like ninja files)
                    if let btree_map::Entry::Occupied(existing_entry) = &existing_entry {
                        let inode = &inodes[existing_entry.get()];
                        if inode.target.as_deref() == Some(target) {
                            continue;
                        }
                    }

                    let mut stat: libc::stat64 = unsafe { mem::zeroed() };
                    stat.st_ino = next_ino;
                    stat.st_dev = FAKE_DEV_ID;
                    stat.st_nlink = 1;
                    stat.st_mode = libc::S_IFLNK | 0o444;
                    stat.st_uid = uid;
                    stat.st_gid = gid;
                    stat.st_size = target.len() as i64;
                    stat.st_blksize = 4096;
                    stat.st_blocks = 1;
                    stat.st_mtime = time;
                    stat.st_atime = time;
                    stat.st_ctime = time;
                    inodes.insert(
                        next_ino,
                        Inode {
                            stat,
                            subpath: existing_entry.key().to_owned(),
                            content: None,
                            executable: false,
                            target: Some(target.to_owned()),
                        },
                    );
                }
                FileEntryRef::Directory => {
                    // Skip creating duplicate entries (these are common when building depmaps from things like ninja files)
                    if let btree_map::Entry::Occupied(existing_entry) = &existing_entry {
                        let inode = &inodes[existing_entry.get()];
                        if inode.stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                            continue;
                        }
                    }

                    let mut stat: libc::stat64 = unsafe { mem::zeroed() };
                    stat.st_ino = next_ino;
                    stat.st_dev = FAKE_DEV_ID;
                    stat.st_nlink = 2;
                    stat.st_mode = libc::S_IFDIR | 0o777;
                    stat.st_uid = uid;
                    stat.st_gid = gid;
                    stat.st_size = 4096;
                    stat.st_blksize = 4096;
                    stat.st_blocks = 1;
                    stat.st_mtime = time;
                    stat.st_atime = time;
                    stat.st_ctime = time;
                    inodes.insert(
                        next_ino,
                        Inode {
                            stat,
                            subpath: existing_entry.key().to_owned(),
                            content: None,
                            executable: false,
                            target: None,
                        },
                    );
                }
            }
            match existing_entry {
                btree_map::Entry::Occupied(mut existing) => {
                    existing.insert(next_ino);
                }
                btree_map::Entry::Vacant(vacant) => {
                    vacant.insert(next_ino);
                }
            }
            next_ino += 1;
        }

        span.record("inode_count", &next_ino);
    }

    let server = Arc::new(Server::new(DepmapFilesystem {
        context: context.clone(),
        depmap,
        entries,
        uid,
        gid,
        time,

        inodes,
        handles: Default::default(),
        next_handle: AtomicU64::new(1),
    }));

    let mut session = FuseSession::new_with_autounmount(mount_dir, "cealn-depmap", "", true, false).context("fuse session create failed")?;
    session.mount().context("fuse mount failed")?;
    let session = Arc::new(Mutex::new(session));

    let mut runners = Vec::new();
    for _ in 0..1 {
        let channel = session.lock().unwrap().new_channel().context("fuse session channel create failed")?;
        runners.push(std::thread::Builder::new().name("cealn-fuse".to_owned()).spawn({
            let server = server.clone();
            let session = session.clone();
            move || run_service(server, session, channel)
        })?);
    }
    Ok(Mount {
        session,
        runners,
        unmounted: false,
    })
}

impl Mount {
    pub fn shutdown(mut self) -> Result<()> {
        let mut session = self.session.lock().unwrap();
        session.wake()?;
        // session.umount()?;
        self.unmounted = true;
        for runner in self.runners.drain(..) {
            runner.join().unwrap()?;
        }
        Ok(())
    }
}

fn run_service<C: ActionContext>(
    server: Arc<Server<DepmapFilesystem<C>>>,
    session: Arc<Mutex<FuseSession>>,
    channel: FuseChannel,
) -> Result<()> {
    match panic::catch_unwind(AssertUnwindSafe(move || do_run_service(server, channel))) {
        Ok(result) => result,
        Err(_panic) => {
            let _ = session.lock().unwrap().umount();
            std::process::abort();
        }
    }
}

fn do_run_service<C: ActionContext>(server: Arc<Server<DepmapFilesystem<C>>>, mut channel: FuseChannel) -> Result<()> {
    while let Some((reader, writer)) = channel.get_request()? {
        match server.handle_message(reader, Writer::FuseDev(writer), None, None) {
            Ok(_) => {}
            Err(fuse_backend_rs::Error::EncodeMessage(ref err)) if err.raw_os_error() == Some(libc::EBADF) => {
                break;
            }
            Err(err) => return Err(err.into()),
        }
    }

    Ok(())
}

struct DepmapFilesystem<C: ActionContext> {
    context: C,

    depmap: ConcreteFiletree,
    entries: BTreeMap<NormalizedDescending<LabelPathBuf>, u64>,
    uid: libc::uid_t,
    gid: libc::gid_t,
    time: libc::time_t,

    inodes: HashMap<u64, Inode>,
    handles: DashMap<u64, Handle>,
    next_handle: AtomicU64,
}

struct Inode {
    stat: libc::stat64,
    subpath: NormalizedDescending<LabelPathBuf>,
    content: Option<FileHash>,
    executable: bool,
    target: Option<String>,
}

struct Handle {
    inode: u64,
    inner: Option<std::fs::File>,
}

impl<C: ActionContext> FileSystem for DepmapFilesystem<C> {
    type Inode = u64;

    type Handle = u64;

    fn init(&self, capable: FsOptions) -> io::Result<FsOptions> {
        Ok(FsOptions::empty())
    }

    fn destroy(&self) {}

    fn lookup(&self, ctx: &Context, parent: Self::Inode, name: &std::ffi::CStr) -> io::Result<Entry> {
        let span = trace_span!("lookup", ?name, parent.subpath = tracing::field::Empty);
        let _gaurd = span.enter();
        let inode = self
            .inodes
            .get(&parent)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        span.record("parent.subpath", tracing::field::display(&inode.subpath));
        if !(inode.stat.st_mode & libc::S_IFMT == libc::S_IFDIR) {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        let Some(filename) = std::str::from_utf8(name.to_bytes())
            .ok()
            .and_then(|name| LabelPath::new(name).ok())
            .and_then(|x| x.require_normalized_descending()) else {
            trace!("unsuitable filename");
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        };
        let subpath = inode.subpath.join(filename);
        let Some(entry_inode_nr) = self.entries.get(&subpath) else {
            trace!("no such entry");
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        };
        let entry_inode = &self.inodes[entry_inode_nr];
        let stat = self.get_stat(entry_inode)?;
        Ok(Entry {
            inode: *entry_inode_nr,
            generation: 0,
            attr: stat,
            attr_flags: 0,
            attr_timeout: ATTR_TIMEOUT,
            entry_timeout: ATTR_TIMEOUT,
        })
    }

    fn open(
        &self,
        ctx: &Context,
        inode_nr: Self::Inode,
        flags: u32,
        fuse_flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        let span = trace_span!("open", subpath = tracing::field::Empty);
        let _guard = span.enter();
        if flags & libc::O_WRONLY as u32 != 0 {
            trace!("attempted to open file with write access");
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        }
        let inode = self
            .inodes
            .get(&inode_nr)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        span.record("subpath", tracing::field::display(&inode.subpath));
        if inode.stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            trace!("attempted to open directory as file");
            return Err(io::Error::from_raw_os_error(libc::EISDIR));
        }
        let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
        let path = self
            .block_on({
                let context = self.context.clone();
                let content = inode.content.clone().unwrap();
                let executable = inode.executable;
                async move {
                    let guard = context.open_cache_file(content.as_ref(), executable).await?;
                    // FIXME: keep guard alive
                    anyhow::Result::Ok((*guard).to_owned())
                }
            })
            .map_err::<io::Error, _>(|_: anyhow::Error| todo!())?;
        let rebased_path =
            Path::new(CACHE_MOUNT_PATH).join(path.strip_prefix(self.context.primary_cache_dir()).unwrap());
        let file = std::fs::File::open(&rebased_path)?;
        self.handles.insert(handle, Handle::new_reg(inode_nr, file));
        Ok((Some(handle), OpenOptions::empty()))
    }

    fn opendir(
        &self,
        ctx: &Context,
        inode_nr: Self::Inode,
        flags: u32,
    ) -> io::Result<(Option<Self::Handle>, OpenOptions)> {
        let inode = self
            .inodes
            .get(&inode_nr)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        if inode.stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
            let handle = self.next_handle.fetch_add(1, Ordering::Relaxed);
            self.handles.insert(handle, Handle::new_dir(inode_nr));
            Ok((Some(handle), OpenOptions::empty()))
        } else {
            Err(io::Error::from_raw_os_error(libc::ENOTDIR))
        }
    }

    fn read(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        w: &mut dyn ZeroCopyWriter,
        size: u32,
        offset: u64,
        lock_owner: Option<u64>,
        flags: u32,
    ) -> io::Result<usize> {
        let span = trace_span!("read", size, offset, subpath = tracing::field::Empty);
        let _guard = span.enter();
        let mut handle = self
            .handles
            .get_mut(&handle)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        if tracing::enabled!(tracing::Level::TRACE) {
            let inode = &self.inodes[&handle.inode];
            span.record("subpath", tracing::field::display(&inode.subpath));
        }
        let file = handle
            .inner
            .as_mut()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        w.write_from(file, size as usize, offset)
    }

    fn readlink(&self, ctx: &Context, inode_nr: Self::Inode) -> io::Result<Vec<u8>> {
        let inode = self
            .inodes
            .get(&inode_nr)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        if inode.stat.st_mode & libc::S_IFMT != libc::S_IFLNK {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        Ok(inode.target.clone().unwrap().into_bytes())
    }

    fn readdir(
        &self,
        ctx: &Context,
        inode_nr: Self::Inode,
        handle: Self::Handle,
        size: u32,
        offset: u64,
        add_entry: &mut dyn FnMut(DirEntry) -> io::Result<usize>,
    ) -> io::Result<()> {
        let span = trace_span!("readdir", subpath = tracing::field::Empty);
        let _guard = span.enter();
        let inode = self
            .inodes
            .get(&inode_nr)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        span.record("subpath", tracing::field::display(&inode.subpath));
        if inode.stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
        }
        let entries = if inode_nr != 1 {
            let mut next_path = inode.subpath.as_str().to_owned();
            // Character after forward slash
            next_path.push('\x30');
            let next_path = LabelPath::new(&next_path)
                .unwrap()
                .normalize_require_descending()
                .unwrap()
                .into_owned();
            self.entries
                .range::<NormalizedDescending<LabelPathBuf>, _>(&inode.subpath..&next_path)
                .enumerate()
                .skip(offset as usize)
        } else {
            self.entries
                .range::<NormalizedDescending<LabelPathBuf>, _>(..)
                .enumerate()
                .skip(offset as usize)
        };
        for (i, (name, entry_inode_nr)) in entries {
            let Some(subpath_within) = name.strip_prefix(&inode.subpath) else {
                continue;
            };
            if subpath_within.as_str().contains('/') || subpath_within.as_str().is_empty() {
                continue;
            }
            let entry_inode = &self.inodes[entry_inode_nr];
            let type_ = if entry_inode.stat.st_mode & libc::S_IFMT == libc::S_IFREG {
                libc::DT_REG as u32
            } else if entry_inode.stat.st_mode & libc::S_IFMT == libc::S_IFLNK {
                libc::DT_LNK as u32
            } else if entry_inode.stat.st_mode & libc::S_IFMT == libc::S_IFDIR {
                libc::DT_DIR as u32
            } else {
                unreachable!()
            };
            trace!(name = %name.file_name().unwrap(), "entry");
            let bytes_written = add_entry(DirEntry {
                ino: *entry_inode_nr,
                offset: (i + 1) as u64,
                type_,
                name: name.file_name().unwrap().as_str().as_bytes(),
            })?;
            if bytes_written == 0 {
                // Out of space in buffer
                break;
            }
        }
        Ok(())
    }

    fn readdirplus(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        size: u32,
        offset: u64,
        add_entry: &mut dyn FnMut(DirEntry, fuse_backend_rs::api::filesystem::Entry) -> io::Result<usize>,
    ) -> io::Result<()> {
        todo!()
    }

    fn access(&self, ctx: &Context, inode: Self::Inode, mask: u32) -> io::Result<()> {
        todo!()
    }

    fn lseek(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        offset: u64,
        whence: u32,
    ) -> io::Result<u64> {
        let span = trace_span!("read", whence, offset, subpath = tracing::field::Empty);
        let _guard = span.enter();
        let mut handle = self
            .handles
            .get_mut(&handle)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        if tracing::enabled!(tracing::Level::TRACE) {
            let inode = &self.inodes[&handle.inode];
            span.record("subpath", tracing::field::display(&inode.subpath));
        }
        let file = handle
            .inner
            .as_mut()
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        let seek_from = match whence as c_int {
            libc::SEEK_SET => SeekFrom::Start(offset),
            libc::SEEK_CUR => SeekFrom::Current(offset as i64),
            libc::SEEK_END => SeekFrom::End(offset as i64),
            _ => return Err(io::Error::from_raw_os_error(libc::EINVAL)),
        };
        file.seek(seek_from)
    }

    fn release(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        flags: u32,
        handle: Self::Handle,
        flush: bool,
        flock_release: bool,
        lock_owner: Option<u64>,
    ) -> io::Result<()> {
        self.handles.remove(&handle);
        Ok(())
    }

    fn releasedir(&self, ctx: &Context, inode: Self::Inode, flags: u32, handle: Self::Handle) -> io::Result<()> {
        self.handles.remove(&handle);
        Ok(())
    }

    fn statfs(&self, ctx: &Context, inode: Self::Inode) -> io::Result<libc::statvfs64> {
        let mut stat: libc::statvfs64 = unsafe { mem::zeroed() };

        stat.f_namemax = 255;
        stat.f_bsize = 4096;

        Ok(stat)
    }

    fn getattr(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Option<Self::Handle>,
    ) -> io::Result<(libc::stat64, std::time::Duration)> {
        let inode = self
            .inodes
            .get(&inode)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::EBADF))?;
        let stat = self.get_stat(inode)?;
        Ok((stat, ATTR_TIMEOUT))
    }

    fn getxattr(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        name: &std::ffi::CStr,
        size: u32,
    ) -> io::Result<GetxattrReply> {
        let span = trace_span!("getxattr", ?name);
        let _guard = span.enter();
        Err(io::Error::from_raw_os_error(libc::ENODATA))
    }

    fn listxattr(&self, ctx: &Context, inode: Self::Inode, size: u32) -> io::Result<ListxattrReply> {
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn setattr(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        attr: libc::stat64,
        handle: Option<Self::Handle>,
        valid: fuse_backend_rs::api::filesystem::SetattrValid,
    ) -> io::Result<(libc::stat64, std::time::Duration)> {
        error!("setattr attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn symlink(
        &self,
        ctx: &Context,
        linkname: &std::ffi::CStr,
        parent: Self::Inode,
        name: &std::ffi::CStr,
    ) -> io::Result<fuse_backend_rs::api::filesystem::Entry> {
        error!("symlink attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn mknod(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        name: &std::ffi::CStr,
        mode: u32,
        rdev: u32,
        umask: u32,
    ) -> io::Result<fuse_backend_rs::api::filesystem::Entry> {
        error!("mknod attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn mkdir(
        &self,
        ctx: &Context,
        parent: Self::Inode,
        name: &std::ffi::CStr,
        mode: u32,
        umask: u32,
    ) -> io::Result<fuse_backend_rs::api::filesystem::Entry> {
        error!("mkdir attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn unlink(&self, ctx: &Context, parent: Self::Inode, name: &std::ffi::CStr) -> io::Result<()> {
        error!("unlink attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn rmdir(&self, ctx: &Context, parent: Self::Inode, name: &std::ffi::CStr) -> io::Result<()> {
        error!("rmdir attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn rename(
        &self,
        ctx: &Context,
        olddir: Self::Inode,
        oldname: &std::ffi::CStr,
        newdir: Self::Inode,
        newname: &std::ffi::CStr,
        flags: u32,
    ) -> io::Result<()> {
        error!("rename attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn link(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        newparent: Self::Inode,
        newname: &std::ffi::CStr,
    ) -> io::Result<fuse_backend_rs::api::filesystem::Entry> {
        error!("link attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn create(
        &self,
        ctx: &Context,
        parent: Self::Inode,
        name: &std::ffi::CStr,
        args: fuse_backend_rs::abi::fuse_abi::CreateIn,
    ) -> io::Result<(
        fuse_backend_rs::api::filesystem::Entry,
        Option<Self::Handle>,
        OpenOptions,
    )> {
        error!("create attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn write(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        r: &mut dyn fuse_backend_rs::api::filesystem::ZeroCopyReader,
        size: u32,
        offset: u64,
        lock_owner: Option<u64>,
        delayed_write: bool,
        flags: u32,
        fuse_flags: u32,
    ) -> io::Result<usize> {
        error!("write attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn flush(&self, ctx: &Context, inode: Self::Inode, handle: Self::Handle, lock_owner: u64) -> io::Result<()> {
        Ok(())
    }

    fn fsync(&self, ctx: &Context, inode: Self::Inode, datasync: bool, handle: Self::Handle) -> io::Result<()> {
        Ok(())
    }

    fn fallocate(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        mode: u32,
        offset: u64,
        length: u64,
    ) -> io::Result<()> {
        error!("fallocate attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn setxattr(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        name: &std::ffi::CStr,
        value: &[u8],
        flags: u32,
    ) -> io::Result<()> {
        error!("setxattr attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn removexattr(&self, ctx: &Context, inode: Self::Inode, name: &std::ffi::CStr) -> io::Result<()> {
        error!("removexattr attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn fsyncdir(&self, ctx: &Context, inode: Self::Inode, datasync: bool, handle: Self::Handle) -> io::Result<()> {
        Ok(())
    }

    fn getlk(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        owner: u64,
        lock: fuse_backend_rs::api::filesystem::FileLock,
        flags: u32,
    ) -> io::Result<fuse_backend_rs::api::filesystem::FileLock> {
        error!("getlk attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn setlk(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        owner: u64,
        lock: fuse_backend_rs::api::filesystem::FileLock,
        flags: u32,
    ) -> io::Result<()> {
        error!("setlk attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn setlkw(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        owner: u64,
        lock: fuse_backend_rs::api::filesystem::FileLock,
        flags: u32,
    ) -> io::Result<()> {
        error!("setlkw attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::EACCES))
    }

    fn ioctl(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        flags: u32,
        cmd: u32,
        data: fuse_backend_rs::api::filesystem::IoctlData,
        out_size: u32,
    ) -> io::Result<fuse_backend_rs::api::filesystem::IoctlData> {
        error!("ioctl attempted on depmap fs");
        // Rather than ENOSYS, let's return ENOTTY so simulate that the ioctl call is implemented
        // but no ioctl number is supported.
        Err(io::Error::from_raw_os_error(libc::ENOTTY))
    }

    fn bmap(&self, ctx: &Context, inode: Self::Inode, block: u64, blocksize: u32) -> io::Result<u64> {
        error!("bmap attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn poll(
        &self,
        ctx: &Context,
        inode: Self::Inode,
        handle: Self::Handle,
        khandle: Self::Handle,
        flags: u32,
        events: u32,
    ) -> io::Result<u32> {
        error!("poll attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }

    fn notify_reply(&self) -> io::Result<()> {
        error!("notify_reply attempted on depmap fs");
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }
}

impl<C: ActionContext> DepmapFilesystem<C> {
    fn block_on<F>(&self, f: F) -> F::Output
    where
        F: Future + Send + 'static,
        F::Output: Send,
    {
        futures::executor::block_on(self.context.spawn_immediate(f))
    }

    fn get_stat(&self, inode: &Inode) -> io::Result<libc::stat64> {
        let mut stat = inode.stat.clone();
        if stat.st_mode & libc::S_IFMT == libc::S_IFREG {
            // We fill in st_size and st_blocks lazily
            let metadata = self
                .block_on({
                    let context = self.context.clone();
                    let content = inode.content.clone().unwrap();
                    let executable = inode.executable;
                    async move {
                        let cachefile = context.open_cache_file(content.as_ref(), executable).await?;
                        let metadata = compio_fs::symlink_metadata(&*cachefile).await?;
                        anyhow::Result::Ok(metadata)
                    }
                })
                .map_err::<io::Error, _>(|_: anyhow::Error| todo!())?;
            stat.st_size = metadata.len() as i64;
            stat.st_blocks = ((metadata.len() + stat.st_blksize as u64 - 1) / stat.st_blksize as u64) as i64;
        }
        Ok(stat)
    }
}

impl Handle {
    fn new_dir(inode: u64) -> Handle {
        Handle { inode, inner: None }
    }

    fn new_reg(inode: u64, inner: std::fs::File) -> Handle {
        Handle {
            inode,
            inner: Some(inner),
        }
    }
}

const FAKE_DEV_ID: u64 = 133713371337;
const ATTR_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);
