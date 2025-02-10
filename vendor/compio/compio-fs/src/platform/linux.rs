use std::{
    borrow::Cow,
    ffi::{c_uchar, c_ushort, CString, OsStr},
    fmt,
    fs::File as StdFile,
    io::{self, Result, Seek, SeekFrom},
    mem::{self, MaybeUninit},
    os::{
        fd::{AsRawFd, RawFd},
        raw::c_int,
        unix::prelude::OsStrExt,
    },
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::{Duration, SystemTime},
};

use compio_core::{
    buffer::{RawInputBuffer, RawOutputBuffer},
    os::linux::{EventQueueExt, EventQueueKind},
    EventQueue,
};
use compio_internal_util::libc_call;
use futures::{channel::mpsc, future::RemoteHandle, FutureExt, SinkExt, StreamExt, TryStreamExt};
use libc::{c_short, c_uint, ino64_t, off64_t};

use crate::platform_unix;

#[cfg(feature = "io-uring")]
#[path = "linux/io_uring.rs"]
mod io_uring;

pub(crate) struct File {
    fd: StdFile,
    imp: FileImp,
}

enum FileImp {
    #[cfg(feature = "io-uring")]
    IoUring(self::io_uring::File),
}

pub(crate) struct Directory {
    fd: Arc<StdFile>,
    imp: DirectoryImp,
}

enum DirectoryImp {
    #[cfg(feature = "io-uring")]
    IoUring(self::io_uring::Directory),
}

pub(crate) struct ReadDir {
    receiver: mpsc::Receiver<Result<DirEntry>>,
    _handle: RemoteHandle<()>,
}

pub(crate) struct DirEntry {
    imp: libc::dirent64,
    namelen: usize,
}

pub(crate) struct FileType {
    d_type: c_uchar,
}

pub(crate) struct OpenOptions {
    tmpfile: bool,
    nofollow: bool,
    only_path: bool,
    mode: c_uint,
}

impl File {
    pub(crate) async fn open_with_options(options: &crate::OpenOptions, path: &Path) -> Result<File> {
        EventQueue::with_current(|queue| {
            match queue.kind() {
                #[cfg(feature = "io-uring")]
                EventQueueKind::IoUring(io_uring)
                    if io_uring.probe().is_supported(::io_uring::opcode::OpenAt::CODE) =>
                {
                    return io_uring::File::open_with_options(options, path);
                }
                _ => {}
            }

            // Fallback to thread pool
            todo!()
        })
        .await
    }

    #[inline]
    pub async fn read<'a>(&'a mut self, buffer: impl RawInputBuffer + 'a) -> Result<usize> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            FileImp::IoUring(imp) => imp.read(&self.fd, buffer).await,
        }
    }

    #[inline]
    pub async fn write<'a>(&'a mut self, buffer: impl RawOutputBuffer + 'a) -> Result<usize> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            FileImp::IoUring(imp) => imp.write(&self.fd, buffer).await,
        }
    }

    #[inline]
    pub async fn seek<'a>(&'a mut self, pos: SeekFrom) -> Result<u64> {
        // TODO: consider not using the native cursor for io_uring backed files. io_uring supports pread and we could
        // manage it in userspace to make "seek" operations faster.
        self.fd.seek(pos)
    }

    #[inline]
    pub async fn symlink_metadata<'a>(&'a mut self) -> Result<Metadata> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            FileImp::IoUring(imp) => imp.symlink_metadata(&self.fd).await,
        }
    }
}

pub trait FileExt {
    fn as_raw_fd(&self) -> RawFd;
}

impl FileExt for crate::File {
    #[inline]
    fn as_raw_fd(&self) -> RawFd {
        self.imp.fd.as_raw_fd()
    }
}

impl Directory {
    pub(crate) fn clone(&self) -> Result<Directory> {
        Ok(Directory {
            fd: self.fd.clone(),
            imp: match &self.imp {
                #[cfg(feature = "io-uring")]
                DirectoryImp::IoUring(imp) => DirectoryImp::IoUring(imp.clone()),
            },
        })
    }

    pub(crate) async fn open(path: &Path) -> Result<Directory> {
        EventQueue::with_current(|queue| {
            match queue.kind() {
                #[cfg(feature = "io-uring")]
                EventQueueKind::IoUring(io_uring)
                    if io_uring.probe().is_supported(::io_uring::opcode::OpenAt::CODE) =>
                {
                    return io_uring::Directory::open(path);
                }
                _ => {}
            }

            // Fallback to thread pool
            todo!()
        })
        .await
    }

    pub(crate) async fn create(path: &Path) -> Result<()> {
        EventQueue::with_current(|queue| {
            match queue.kind() {
                #[cfg(feature = "io-uring")]
                EventQueueKind::IoUring(io_uring)
                    if io_uring.probe().is_supported(::io_uring::opcode::MkDirAt::CODE) =>
                {
                    return io_uring::Directory::create(path);
                }
                _ => {}
            }

            // Fallback to thread pool
            todo!()
        })
        .await
    }

    pub(crate) async fn create_all(path: &Path) -> Result<()> {
        if path.is_relative() {
            todo!()
        }

        // We may retry the entire process in the event of certain race conditions, but only a limited number of times
        'full_retry: for _ in 0..16 {
            let mut ancestor_depth = 0usize;
            for ancestor in path.ancestors() {
                match crate::Directory::create(ancestor).await {
                    Ok(()) => break,
                    Err(ref err) if err.kind() == io::ErrorKind::AlreadyExists => {
                        break;
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::NotFound => {
                        ancestor_depth += 1;
                    }
                    Err(err) => return Err(err),
                }
            }

            'create_forward: loop {
                let ancestor = path.ancestors().nth(ancestor_depth).unwrap();
                match crate::Directory::create(ancestor).await {
                    Ok(()) => {
                        if ancestor_depth == 0 {
                            break 'create_forward;
                        }
                        ancestor_depth -= 1;
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::AlreadyExists => {
                        if ancestor_depth == 0 {
                            break 'create_forward;
                        }
                        ancestor_depth -= 1;
                        continue 'create_forward;
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::NotFound => {
                        // Directory must have been deleted in a a race
                        continue 'full_retry;
                    }
                    Err(err) => return Err(err),
                }
            }

            return Ok(());
        }

        Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "failed to converge on created directory state, another process may be modifying the directory tree"
            ),
        ))
    }

    pub(crate) async fn open_at_file_with_options(
        &mut self,
        options: &crate::OpenOptions,
        path: &Path,
    ) -> Result<File> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            DirectoryImp::IoUring(imp) => imp.open_at_file_with_options(&self.fd, options, path).await,
        }
    }

    pub(crate) async fn open_at_directory(&mut self, path: &Path) -> Result<Directory> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            DirectoryImp::IoUring(imp) => imp.open_at_directory(&self.fd, path).await,
        }
    }

    pub(crate) async fn create_at_directory(&mut self, path: &Path) -> Result<()> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            DirectoryImp::IoUring(imp) => imp.create_at_directory(&self.fd, path).await,
        }
    }

    pub(crate) async fn create_at_directory_all(&mut self, path: &Path) -> Result<()> {
        if path.as_os_str().is_empty() {
            return Ok(());
        }

        // We may retry the entire process in the event of certain race conditions, but only a limited number of times
        'full_retry: for _ in 0..16 {
            let mut ancestor_depth = 0usize;
            for ancestor in path.ancestors() {
                match self.create_at_directory(ancestor).await {
                    Ok(()) => break,
                    Err(ref err) if err.kind() == io::ErrorKind::AlreadyExists => {
                        break;
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::NotFound => {
                        ancestor_depth += 1;
                    }
                    Err(err) => return Err(err),
                }
            }

            'create_forward: loop {
                let ancestor = path.ancestors().nth(ancestor_depth).unwrap();
                match self.create_at_directory(ancestor).await {
                    Ok(()) => {
                        if ancestor_depth == 0 {
                            break 'create_forward;
                        }
                        ancestor_depth -= 1;
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::AlreadyExists => {
                        if ancestor_depth == 0 {
                            break 'create_forward;
                        }
                        ancestor_depth -= 1;
                        continue 'create_forward;
                    }
                    Err(ref err) if err.kind() == io::ErrorKind::NotFound => {
                        // Directory must have been deleted in a a race
                        continue 'full_retry;
                    }
                    Err(err) => return Err(err),
                }
            }

            return Ok(());
        }

        Err(io::Error::new(
            io::ErrorKind::Other,
            format!(
                "failed to converge on created directory state, another process may be modifying the directory tree"
            ),
        ))
    }

    pub(crate) async fn link_at(&mut self, link_path: &Path, target_path: &Path) -> Result<()> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            DirectoryImp::IoUring(imp) => imp.link_at(&self.fd, link_path, target_path).await,
        }
    }

    pub(crate) async fn symlink_at(&mut self, link_path: &Path, target_path: &Path) -> Result<()> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            DirectoryImp::IoUring(imp) => imp.symlink_at(&self.fd, link_path, target_path).await,
        }
    }

    pub(crate) async fn unlink_at(&mut self, path: &Path) -> Result<()> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            DirectoryImp::IoUring(imp) => imp.unlink_at(&self.fd, path).await,
        }
    }

    pub(crate) async fn remove_dir_at(&mut self, path: &Path) -> Result<()> {
        match &mut self.imp {
            #[cfg(feature = "io-uring")]
            DirectoryImp::IoUring(imp) => imp.remove_dir_at(&self.fd, path).await,
        }
    }

    pub(crate) async fn read_link_at(&mut self, path: &Path) -> Result<PathBuf> {
        compio_executor::block(async {
            unsafe {
                let mut buffer = [0u8; libc::PATH_MAX as usize];
                // TODO: don't allocate here, use a stack buffer
                let path_cstr = CString::new(path.as_os_str().as_bytes())?;
                let target_len = libc_call!(libc::readlinkat(
                    self.fd.as_raw_fd(),
                    path_cstr.as_ptr(),
                    buffer.as_mut_ptr() as _,
                    buffer.len(),
                ))?;
                Ok(PathBuf::from(
                    OsStr::from_bytes(&buffer[..target_len as usize]).to_owned(),
                ))
            }
        })
        .await
    }

    pub(crate) async fn symlink_metadata_at(&mut self, path: &Path) -> Result<Metadata> {
        // TODO: if io_uring is not in use, just do a fstatat
        // TODO: do this without allocating an extra file object, just do it at the file descriptor level
        let mut file = self
            .open_at_file_with_options(crate::OpenOptions::new().only_path(true).nofollow(true), path)
            .await?;
        file.symlink_metadata().await
    }

    pub(crate) async fn read_dir(&mut self) -> Result<ReadDir> {
        // io_uring currently doesn't support getdents
        // TODO: don't pull buffer size out of our ass
        let (tx, rx) = mpsc::channel(64);
        let handle = compio_executor::spawn_blocking_handle(do_read_dir(self.fd.as_raw_fd(), tx));

        Ok(ReadDir {
            receiver: rx,
            _handle: handle,
        })
    }

    pub(crate) async fn remove_all(mut this: &mut crate::Directory) -> Result<()> {
        let mut scan_directories = vec![this.clone()?];
        let mut delete_directories = Vec::new();
        while let Some(mut scan_directory) = scan_directories.pop() {
            let mut scan_directory_clone = scan_directory.clone().unwrap();
            let mut read_dir = scan_directory_clone.read_dir().await?;
            while let Some(entry) = read_dir.try_next().await? {
                if entry.file_type().await?.is_dir() {
                    let subdir = scan_directory.open_at_directory(&entry.file_name()).await?;
                    scan_directories.push(subdir);
                    delete_directories.push(RemoveAllStackEntry::Subdir(entry));
                } else {
                    scan_directory.unlink_at(entry.file_name()).await?;
                }
            }
            delete_directories.push(RemoveAllStackEntry::Directory(scan_directory));
        }
        let mut current_directory = None;
        while let Some(stack_entry) = delete_directories.pop() {
            match stack_entry {
                RemoveAllStackEntry::Directory(directory) => {
                    current_directory = Some(directory);
                }
                RemoveAllStackEntry::Subdir(entry) => {
                    current_directory
                        .as_mut()
                        .unwrap()
                        .remove_dir_at(entry.file_name())
                        .await?;
                }
            }
        }
        Ok(())
    }
}

enum RemoveAllStackEntry {
    Subdir(crate::DirEntry),
    Directory(crate::Directory),
}

pub(crate) async fn remove_file(path: &Path) -> Result<()> {
    EventQueue::with_current(|queue| {
        match queue.kind() {
            #[cfg(feature = "io-uring")]
            EventQueueKind::IoUring(io_uring) if io_uring.probe().is_supported(::io_uring::opcode::UnlinkAt::CODE) => {
                return io_uring::remove_file(path);
            }
            _ => {}
        }

        // Fallback to thread pool
        todo!()
    })
    .await
}

pub(crate) async fn rename(src: &Path, dest: &Path) -> Result<()> {
    EventQueue::with_current(|queue| {
        match queue.kind() {
            #[cfg(feature = "io-uring")]
            EventQueueKind::IoUring(io_uring) if io_uring.probe().is_supported(::io_uring::opcode::RenameAt::CODE) => {
                return io_uring::rename(src, dest);
            }
            _ => {}
        }

        // Fallback to thread pool
        todo!()
    })
    .await
}

pub(crate) async fn symlink_metadata(path: &Path) -> Result<Metadata> {
    EventQueue::with_current(|queue| {
        match queue.kind() {
            #[cfg(feature = "io-uring")]
            EventQueueKind::IoUring(io_uring) if io_uring.probe().is_supported(::io_uring::opcode::Statx::CODE) => {
                return io_uring::symlink_metadata(path);
            }
            _ => {}
        }

        // Fallback to thread pool
        todo!()
    })
    .await
}

pub(crate) async fn remove_dir(path: &Path) -> Result<()> {
    EventQueue::with_current(|queue| {
        match queue.kind() {
            #[cfg(feature = "io-uring")]
            EventQueueKind::IoUring(io_uring) if io_uring.probe().is_supported(::io_uring::opcode::UnlinkAt::CODE) => {
                return io_uring::remove_dir(path);
            }
            _ => {}
        }

        // Fallback to thread pool
        todo!()
    })
    .await
}

impl ReadDir {
    #[inline]
    pub(crate) fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Result<DirEntry>>> {
        self.get_mut().receiver.poll_next_unpin(cx)
    }
}

impl DirEntry {
    #[inline]
    pub(crate) fn file_name(&self) -> Cow<OsStr> {
        Cow::Borrowed(OsStr::from_bytes(unsafe {
            std::slice::from_raw_parts(self.imp.d_name.as_ptr() as *const u8, self.namelen)
        }))
    }

    #[inline]
    pub(crate) async fn file_type(&self) -> Result<FileType> {
        Ok(FileType {
            d_type: self.imp.d_type,
        })
    }
}

impl FileType {
    #[inline]
    pub(crate) fn is_dir(&self) -> bool {
        self.d_type == libc::DT_DIR
    }

    #[inline]
    pub(crate) fn is_file(&self) -> bool {
        self.d_type == libc::DT_REG
    }

    #[inline]
    pub(crate) fn is_symlink(&self) -> bool {
        self.d_type == libc::DT_LNK
    }
}

async fn do_read_dir(fd: libc::c_int, mut tx: mpsc::Sender<Result<DirEntry>>) {
    // TODO: don't pull buffer size out of our ass
    unsafe {
        let mut buffer: [MaybeUninit<u8>; 4096] = MaybeUninit::uninit_array();
        'syscall: loop {
            let ret = libc::syscall(libc::SYS_getdents64, fd, buffer.as_mut_ptr(), buffer.len());
            if ret == 0 {
                break 'syscall;
            } else if ret < 0 {
                let _ = tx.send(Err(io::Error::from_raw_os_error((-ret) as i32))).await;
                break 'syscall;
            }
            let bytes_read = ret as usize;
            let mut remaining = MaybeUninit::slice_assume_init_ref(&buffer[..bytes_read]);
            while remaining.len() >= mem::size_of::<Dirent64Header>() {
                let entry = {
                    let header = &*(remaining.as_ptr() as *const Dirent64Header);
                    let mut entry = DirEntry {
                        imp: mem::zeroed(),
                        namelen: 0,
                    };
                    entry.imp.d_ino = header.d_ino;
                    entry.imp.d_off = header.d_off;
                    entry.imp.d_reclen = header.d_reclen;
                    entry.imp.d_type = header.d_type;
                    let name_buffer = &remaining[mem::size_of::<Dirent64Header>()..];
                    let (name_len, _) = name_buffer
                        .iter()
                        .enumerate()
                        .find(|(_, b)| **b == 0)
                        .expect("missing null terminator");
                    entry.namelen = name_len;
                    entry.imp.d_name[..name_len]
                        .copy_from_slice(std::slice::from_raw_parts(name_buffer.as_ptr() as _, name_len));
                    remaining = &remaining[(header.d_reclen as usize)..];
                    match &name_buffer[..name_len] {
                        b"." => continue,
                        b".." => continue,
                        _ => entry,
                    }
                };
                if let Err(_) = tx.send(Ok(entry)).await {
                    break 'syscall;
                }
            }
        }
    }
}

#[derive(Debug)]
#[repr(packed)]
struct Dirent64Header {
    d_ino: ino64_t,
    d_off: off64_t,
    d_reclen: c_ushort,
    d_type: c_uchar,
}

impl OpenOptions {
    #[inline]
    pub fn new() -> OpenOptions {
        OpenOptions {
            tmpfile: false,
            nofollow: false,
            only_path: false,
            mode: 0o666,
        }
    }
}

pub trait OpenOptionsExt {
    fn tmpfile(&mut self, tmpfile: bool) -> &mut Self;
    fn mode(&mut self, mode: c_uint) -> &mut Self;

    fn nofollow(&mut self, nofollow: bool) -> &mut Self;
    fn only_path(&mut self, only_path: bool) -> &mut Self;
}

impl OpenOptionsExt for crate::OpenOptions {
    #[inline]
    fn tmpfile(&mut self, tmpfile: bool) -> &mut Self {
        self.imp.tmpfile = tmpfile;
        self
    }

    #[inline]
    fn mode(&mut self, mode: c_uint) -> &mut Self {
        self.imp.mode = mode;
        self
    }

    #[inline]
    fn nofollow(&mut self, nofollow: bool) -> &mut Self {
        self.imp.nofollow = nofollow;
        self
    }

    #[inline]
    fn only_path(&mut self, only_path: bool) -> &mut Self {
        self.imp.only_path = only_path;
        self
    }
}

impl crate::OpenOptions {
    fn to_flags(&self) -> c_int {
        let mut flags = libc::O_CLOEXEC;
        if self.read && self.write {
            flags |= libc::O_RDWR
        } else if self.read {
            flags |= libc::O_RDONLY;
        } else if self.write {
            flags |= libc::O_WRONLY;
        }
        if self.append {
            flags |= libc::O_APPEND;
        }
        if self.truncate {
            flags |= libc::O_TRUNC;
        }
        if self.create || self.create_new {
            flags |= libc::O_CREAT;
        }
        if self.create_new {
            flags |= libc::O_EXCL;
        }
        if self.imp.tmpfile {
            flags |= libc::O_TMPFILE;
        }
        if self.imp.nofollow {
            flags |= libc::O_NOFOLLOW;
        }
        if self.imp.only_path {
            flags |= libc::O_PATH;
        }
        flags
    }

    fn get_mode(&self) -> c_uint {
        self.imp.mode
    }
}

pub(crate) struct Metadata {
    stat: statx,
}

pub(crate) struct Permissions {
    mode: c_uint,
}

pub trait MetadataExt: crate::os::unix::MetadataExt {}

impl Metadata {
    #[inline]
    pub(crate) fn permissions(&self) -> Permissions {
        Permissions {
            mode: self.stat.stx_mode as c_uint,
        }
    }

    #[inline]
    pub(crate) fn len(&self) -> u64 {
        self.stat.stx_size
    }

    #[inline]
    pub(crate) fn is_dir(&self) -> bool {
        self.stat.stx_mode as c_uint & libc::S_IFMT == libc::S_IFDIR
    }

    #[inline]
    pub(crate) fn is_file(&self) -> bool {
        self.stat.stx_mode as c_uint & libc::S_IFMT == libc::S_IFREG
    }

    #[inline]
    pub(crate) fn is_symlink(&self) -> bool {
        self.stat.stx_mode as c_uint & libc::S_IFMT == libc::S_IFLNK
    }

    #[inline]
    pub(crate) fn modified(&self) -> Result<SystemTime> {
        Ok(stx_time_to_systemtime(&self.stat.stx_mtime))
    }
}

impl crate::os::unix::MetadataExt for crate::Metadata {
    fn dev(&self) -> u64 {
        self.imp.stat.stx_mnt_id
    }

    fn ino(&self) -> u64 {
        self.imp.stat.stx_ino
    }
}

impl MetadataExt for crate::Metadata {}

fn stx_time_to_systemtime(time: &statx_timestamp) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::new(time.tv_sec as u64, time.tv_nsec)
}

impl platform_unix::PermissionsExt for crate::Permissions {
    #[inline]
    fn mode(&self) -> u32 {
        self.imp.mode as u32
    }
}

impl fmt::Debug for DirEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirEntry")
            .field("inode", &self.imp.d_ino)
            .field("file_name", &self.file_name())
            .field(
                "file_type",
                &FileType {
                    d_type: self.imp.d_type,
                },
            )
            .finish()
    }
}

impl fmt::Debug for FileType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self.d_type {
            libc::DT_DIR => "FileType::Directory",
            libc::DT_REG => "FileType::Regular",
            libc::DT_LNK => "FileType::Symlink",
            libc::DT_BLK => "FileType::Block",
            libc::DT_CHR => "FileType::Character",
            libc::DT_FIFO => "FileType::Fifo",
            libc::DT_SOCK => "FileType::Socket",
            libc::DT_UNKNOWN => "FileType::Unknown",
            code => return write!(f, "FileType::Invalid({})", code),
        };
        f.write_str(s)
    }
}

// libc crate doesn't define this for musl, so put it here
#[derive(Clone, Copy)]
#[repr(C)]
struct statx {
    pub stx_mask: u32,
    pub stx_blksize: u32,
    pub stx_attributes: u64,
    pub stx_nlink: u32,
    pub stx_uid: u32,
    pub stx_gid: u32,
    pub stx_mode: u16,
    pub stx_ino: u64,
    pub stx_size: u64,
    pub stx_blocks: u64,
    pub stx_attributes_mask: u64,
    pub stx_atime: statx_timestamp,
    pub stx_btime: statx_timestamp,
    pub stx_ctime: statx_timestamp,
    pub stx_mtime: statx_timestamp,
    pub stx_rdev_major: u32,
    pub stx_rdev_minor: u32,
    pub stx_dev_major: u32,
    pub stx_dev_minor: u32,
    pub stx_mnt_id: u64,
    pub stx_dio_mem_align: u32,
    pub stx_dio_offset_align: u32,
}

#[derive(Clone, Copy)]
#[repr(C)]
struct statx_timestamp {
    pub tv_sec: i64,
    pub tv_nsec: u32,
    pub __statx_timestamp_pad1: [i32; 1],
}
