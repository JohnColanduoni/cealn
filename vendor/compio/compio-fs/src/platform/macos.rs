use std::{
    borrow::Cow,
    ffi::{c_char, c_uchar, c_ushort, CString, OsStr, OsString},
    fmt,
    fs::File as StdFile,
    io::{self, Result, Seek, SeekFrom},
    mem::{self, MaybeUninit},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
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
    buffer::{InputBufferVisitor, OutputBufferVisitor, RawInputBuffer, RawOutputBuffer},
    EventQueue,
};
use compio_internal_util::{libc_call, libc_fd_call};
use futures::{channel::mpsc, future::RemoteHandle, FutureExt, SinkExt, StreamExt, TryStreamExt};
use libc::{attrlist, c_short, c_uint};

use crate::platform_unix;

pub(crate) struct File {
    fd: StdFile,
}

pub(crate) struct Directory {
    fd: Arc<StdFile>,
}

pub(crate) struct ReadDir {
    receiver: mpsc::Receiver<Result<DirEntry>>,
    _handle: RemoteHandle<()>,
}

pub(crate) struct DirEntry {
    name: OsString,
    file_type: FileType,
}

#[derive(Clone)]
pub(crate) struct FileType {
    fsobj_type: u32,
}

pub(crate) struct OpenOptions {
    mode: c_ushort,
    nofollow: bool,
    symlink: bool,
}

impl File {
    pub(crate) async fn open_with_options(options: &crate::OpenOptions, path: &Path) -> Result<File> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        let flags = options.to_flags();
        let mode = options.get_mode();
        compio_executor::block(async move {
            unsafe {
                let fd = libc_fd_call!(libc::open(path_cstr.as_ptr(), flags, mode as c_uint))?;
                Ok(File {
                    fd: StdFile::from_raw_fd(fd.into_raw_fd()),
                })
            }
        })
        .await
    }

    #[inline]
    pub async fn read<'a, I>(&'a mut self, buffer: I) -> Result<usize>
    where
        I: RawInputBuffer + 'a,
    {
        struct ReadVisitor {
            fd: RawFd,
        }

        impl InputBufferVisitor for ReadVisitor {
            type Output = Result<isize>;

            fn unpinned_slice(self, buffer: &mut [MaybeUninit<u8>]) -> Self::Output {
                unsafe { libc_call!(libc::read(self.fd, buffer.as_mut_ptr() as _, buffer.len())) }
            }

            fn unpinned_vector(self, buffer: &[&mut [MaybeUninit<u8>]]) -> Self::Output {
                todo!()
            }
        }

        let mut taken = buffer.take();
        let fd = self.fd.as_raw_fd();
        // FIXME: EINTR
        compio_executor::block(async move {
            match I::visit(&mut taken, ReadVisitor { fd }) {
                Ok(len) => {
                    I::finalize(taken, len as usize);
                    Ok(len as usize)
                }
                Err(err) => Err(err),
            }
        })
        .await
    }

    #[inline]
    pub async fn write<'a, O>(&'a mut self, buffer: O) -> Result<usize>
    where
        O: RawOutputBuffer + 'a,
    {
        struct WriteVisitor {
            fd: RawFd,
        }

        impl OutputBufferVisitor for WriteVisitor {
            type Output = Result<isize>;

            fn unpinned_slice(self, data: &[u8]) -> Self::Output {
                unsafe { libc_call!(libc::write(self.fd, data.as_ptr() as _, data.len())) }
            }

            fn unpinned_vector(self, data: &[&[u8]]) -> Self::Output {
                todo!()
            }
        }

        let mut taken = buffer.take();
        let fd = self.fd.as_raw_fd();
        // FIXME: EINTR
        compio_executor::block(async move { O::visit(&mut taken, WriteVisitor { fd }).map(|x| x as usize) }).await
    }

    #[inline]
    pub async fn seek<'a>(&'a mut self, pos: SeekFrom) -> Result<u64> {
        let fd = self.fd.as_raw_fd();
        let (offset, whence) = match pos {
            SeekFrom::Start(offset) => (
                i64::try_from(offset)
                    .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "seek offset did not fit in an i64"))?,
                libc::SEEK_SET,
            ),
            SeekFrom::End(offset) => (offset, libc::SEEK_END),
            SeekFrom::Current(offset) => (offset, libc::SEEK_CUR),
        };
        compio_executor::block(async move { unsafe { libc_call!(libc::lseek(fd, offset, whence)).map(|x| x as u64) } })
            .await
    }

    #[inline]
    pub async fn metadata<'a>(&'a mut self) -> Result<Metadata> {
        let fd = self.fd.as_raw_fd();
        compio_executor::block(async move {
            unsafe {
                let mut stat: libc::stat = mem::zeroed();
                libc_call!(libc::fstat(fd, &mut stat))?;
                Ok(Metadata { stat })
            }
        })
        .await
    }

    #[inline]
    pub async fn symlink_metadata<'a>(&'a mut self) -> Result<Metadata> {
        let fd = self.fd.as_raw_fd();
        compio_executor::block(async move {
            unsafe {
                let mut stat: libc::stat = mem::zeroed();
                libc_call!(libc::fstat(fd, &mut stat))?;
                Ok(Metadata { stat })
            }
        })
        .await
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
        Ok(Directory { fd: self.fd.clone() })
    }

    pub(crate) async fn open(path: &Path) -> Result<Directory> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        compio_executor::block(async move {
            unsafe {
                let fd = libc_fd_call!(libc::open(path_cstr.as_ptr(), libc::O_DIRECTORY | libc::O_CLOEXEC))?;
                Ok(Directory {
                    fd: Arc::new(StdFile::from_raw_fd(fd.into_raw_fd())),
                })
            }
        })
        .await
    }

    pub(crate) async fn create(path: &Path) -> Result<()> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        compio_executor::block(async move {
            unsafe {
                libc_call!(libc::mkdir(path_cstr.as_ptr(), 0o755))?;
                Ok(())
            }
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
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        let fd = self.fd.as_raw_fd();
        let flags = options.to_flags();
        let mode = options.get_mode();
        compio_executor::block(async move {
            unsafe {
                let fd = libc_fd_call!(libc::openat(fd, path_cstr.as_ptr(), flags, mode as c_uint,))?;
                Ok(File {
                    fd: StdFile::from_raw_fd(fd.into_raw_fd()),
                })
            }
        })
        .await
    }

    pub(crate) async fn open_at_directory(&mut self, path: &Path) -> Result<Directory> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        let fd = self.fd.as_raw_fd();
        compio_executor::block(async move {
            unsafe {
                let fd = libc_fd_call!(libc::openat(
                    fd,
                    path_cstr.as_ptr(),
                    libc::O_DIRECTORY | libc::O_CLOEXEC
                ))?;
                Ok(Directory {
                    fd: Arc::new(StdFile::from_raw_fd(fd.into_raw_fd())),
                })
            }
        })
        .await
    }

    pub(crate) async fn create_at_directory(&mut self, path: &Path) -> Result<()> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        let fd = self.fd.as_raw_fd();
        compio_executor::block(async move {
            unsafe {
                libc_call!(libc::mkdirat(fd, path_cstr.as_ptr(), 0o755))?;
                Ok(())
            }
        })
        .await
    }

    pub(crate) async fn create_at_directory_all(&mut self, path: &Path) -> Result<()> {
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
        let link_path_cstr = CString::new(link_path.as_os_str().as_bytes())?;
        let target_path_cstr = CString::new(target_path.as_os_str().as_bytes())?;
        let fd = self.fd.as_raw_fd();
        compio_executor::block(async move {
            unsafe {
                libc_call!(libc::linkat(
                    fd,
                    target_path_cstr.as_ptr(),
                    fd,
                    link_path_cstr.as_ptr(),
                    0
                ))?;
                Ok(())
            }
        })
        .await
    }

    pub(crate) async fn symlink_at(&mut self, link_path: &Path, target_path: &Path) -> Result<()> {
        let link_path_cstr = CString::new(link_path.as_os_str().as_bytes())?;
        let target_path_cstr = CString::new(target_path.as_os_str().as_bytes())?;
        let fd = self.fd.as_raw_fd();
        compio_executor::block(async move {
            unsafe {
                libc_call!(libc::symlinkat(target_path_cstr.as_ptr(), fd, link_path_cstr.as_ptr()))?;
                Ok(())
            }
        })
        .await
    }

    pub(crate) async fn unlink_at(&mut self, path: &Path) -> Result<()> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        let fd = self.fd.as_raw_fd();
        compio_executor::block(async move {
            unsafe {
                libc_call!(libc::unlinkat(fd, path_cstr.as_ptr(), 0))?;
                Ok(())
            }
        })
        .await
    }

    pub(crate) async fn symlink_metadata_at(&mut self, path: &Path) -> Result<Metadata> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        let mut stat: libc::stat = unsafe { mem::zeroed() };
        let fd = self.fd.as_raw_fd();
        compio_executor::block(async move {
            unsafe {
                libc_call!(libc::fstatat(
                    fd,
                    path_cstr.as_ptr(),
                    &mut stat,
                    libc::AT_SYMLINK_NOFOLLOW,
                ))
            }
        })
        .await?;
        Ok(Metadata { stat })
    }

    pub(crate) async fn read_link_at(&mut self, path: &Path) -> Result<PathBuf> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        let mut buffer = [0u8; libc::PATH_MAX as usize];
        let fd = self.fd.as_raw_fd();
        let read_len = compio_executor::block(async move {
            unsafe {
                libc_call!(libc::readlinkat(
                    fd,
                    path_cstr.as_ptr(),
                    buffer.as_mut_ptr() as _,
                    buffer.len()
                ))
            }
        })
        .await?;
        Ok(PathBuf::from(
            OsStr::from_bytes(&buffer[..read_len as usize]).to_owned(),
        ))
    }

    pub(crate) async fn remove_dir_at(&mut self, path: &Path) -> Result<()> {
        let path_cstr = CString::new(path.as_os_str().as_bytes())?;
        let fd = self.fd.as_raw_fd();
        compio_executor::block(async move {
            unsafe {
                libc_call!(libc::unlinkat(fd, path_cstr.as_ptr(), libc::AT_REMOVEDIR))?;
                Ok(())
            }
        })
        .await
    }

    pub(crate) async fn read_dir(&mut self) -> Result<ReadDir> {
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

pub(crate) async fn rename(src: &Path, dest: &Path) -> Result<()> {
    let src_cstr = CString::new(src.as_os_str().as_bytes())?;
    let dest_cstr = CString::new(dest.as_os_str().as_bytes())?;
    compio_executor::block(async move {
        unsafe {
            libc_call!(libc::rename(src_cstr.as_ptr(), dest_cstr.as_ptr()))?;
            Ok(())
        }
    })
    .await
}

pub(crate) async fn symlink_metadata(path: &Path) -> Result<Metadata> {
    let path_cstr = CString::new(path.as_os_str().as_bytes())?;
    compio_executor::block(async move {
        unsafe {
            let mut stat: libc::stat = mem::zeroed();
            libc_call!(libc::lstat(path_cstr.as_ptr(), &mut stat))?;
            Ok(Metadata { stat })
        }
    })
    .await
}

pub(crate) async fn remove_file(path: &Path) -> Result<()> {
    let path_cstr = CString::new(path.as_os_str().as_bytes())?;
    compio_executor::block(async move {
        unsafe {
            libc_call!(libc::unlink(path_cstr.as_ptr()))?;
            Ok(())
        }
    })
    .await
}

pub(crate) async fn remove_dir(path: &Path) -> Result<()> {
    let path_cstr = CString::new(path.as_os_str().as_bytes())?;
    compio_executor::block(async move {
        unsafe {
            libc_call!(libc::rmdir(path_cstr.as_ptr()))?;
            Ok(())
        }
    })
    .await
}

async fn do_read_dir(fd: libc::c_int, mut tx: mpsc::Sender<Result<DirEntry>>) {
    // TODO: don't pull buffer size out of our ass
    unsafe {
        let mut attrlist: libc::attrlist = mem::zeroed();
        attrlist.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        attrlist.commonattr = libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_NAME | libc::ATTR_CMN_OBJTYPE;
        let mut buffer: [MaybeUninit<u8>; 4096] = MaybeUninit::uninit_array();
        'syscall: loop {
            let entry_count = match libc_call!(libc::getattrlistbulk(
                fd,
                &mut attrlist as *mut attrlist as _,
                buffer.as_mut_ptr() as _,
                buffer.len(),
                0
            )) {
                Ok(entry_count) => entry_count,
                Err(err) => {
                    let _ = tx.send(Err(err)).await;
                    break 'syscall;
                }
            };
            if entry_count == 0 {
                break 'syscall;
            }
            let mut list_remaining = MaybeUninit::slice_assume_init_ref(&buffer);
            for _ in 0..entry_count {
                let entry = {
                    let length = *(list_remaining.as_ptr() as *const u32);
                    let mut entry_remaining = &list_remaining[(mem::size_of::<u32>())..(length as usize)];

                    let attribute_set = &*(entry_remaining.as_ptr() as *const libc::attribute_set_t);
                    entry_remaining = &entry_remaining[mem::size_of::<libc::attribute_set_t>()..];
                    list_remaining = &list_remaining[(length as usize)..];

                    if (attribute_set.commonattr & libc::ATTR_CMN_NAME) == 0
                        || (attribute_set.commonattr & libc::ATTR_CMN_OBJTYPE) == 0
                    {
                        continue;
                    }

                    let name_head = entry_remaining;
                    let attr_reference = &*(entry_remaining.as_ptr() as *const libc::attrreference_t);
                    entry_remaining = &entry_remaining[mem::size_of::<libc::attrreference_t>()..];
                    let name = OsStr::from_bytes(
                        &name_head[(attr_reference.attr_dataoffset as usize)..]
                            [..(attr_reference.attr_length as usize - 1)],
                    );

                    let fsobj_type = *(entry_remaining.as_ptr() as *const u32);

                    DirEntry {
                        name: name.to_owned(),
                        file_type: FileType { fsobj_type },
                    }
                };

                if let Err(_) = tx.send(Ok(entry)).await {
                    break 'syscall;
                }
            }
        }
    }
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
        Cow::Borrowed(&self.name)
    }

    #[inline]
    pub(crate) async fn file_type(&self) -> Result<FileType> {
        Ok(self.file_type.clone())
    }
}

impl FileType {
    #[inline]
    pub(crate) fn is_dir(&self) -> bool {
        self.fsobj_type == VDIR
    }

    #[inline]
    pub(crate) fn is_file(&self) -> bool {
        self.fsobj_type == VREG
    }

    #[inline]
    pub(crate) fn is_symlink(&self) -> bool {
        self.fsobj_type == VLNK
    }
}

impl OpenOptions {
    #[inline]
    pub fn new() -> OpenOptions {
        OpenOptions {
            nofollow: false,
            symlink: false,
            mode: 0o666,
        }
    }
}

pub trait OpenOptionsExt {
    fn mode(&mut self, mode: c_ushort) -> &mut Self;

    fn nofollow(&mut self, nofollow: bool) -> &mut Self;
    fn open_symlink(&mut self, symlink: bool) -> &mut Self;
}

impl OpenOptionsExt for crate::OpenOptions {
    #[inline]
    fn mode(&mut self, mode: c_ushort) -> &mut Self {
        self.imp.mode = mode;
        self
    }

    #[inline]
    fn nofollow(&mut self, nofollow: bool) -> &mut Self {
        self.imp.nofollow = nofollow;
        self
    }

    #[inline]
    fn open_symlink(&mut self, symlink: bool) -> &mut Self {
        self.imp.symlink = symlink;
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
        if self.imp.nofollow {
            flags |= libc::O_NOFOLLOW;
        }
        if self.imp.symlink {
            flags |= libc::O_SYMLINK;
        }
        flags
    }

    fn get_mode(&self) -> c_ushort {
        self.imp.mode
    }
}

pub(crate) struct Metadata {
    stat: libc::stat,
}

pub(crate) struct Permissions {
    mode: c_ushort,
}

impl Metadata {
    #[inline]
    pub(crate) fn permissions(&self) -> Permissions {
        Permissions {
            mode: self.stat.st_mode,
        }
    }

    #[inline]
    pub(crate) fn is_file(&self) -> bool {
        self.stat.st_mode & libc::S_IFMT == libc::S_IFREG
    }

    #[inline]
    pub(crate) fn is_dir(&self) -> bool {
        self.stat.st_mode & libc::S_IFMT == libc::S_IFDIR
    }

    #[inline]
    pub(crate) fn is_symlink(&self) -> bool {
        self.stat.st_mode & libc::S_IFMT == libc::S_IFLNK
    }

    #[inline]
    pub(crate) fn modified(&self) -> Result<SystemTime> {
        Ok(SystemTime::UNIX_EPOCH + Duration::new(self.stat.st_mtime as u64, self.stat.st_mtime_nsec as u32))
    }

    #[inline]
    pub(crate) fn len(&self) -> u64 {
        self.stat.st_size as u64
    }
}

impl platform_unix::PermissionsExt for crate::Permissions {
    #[inline]
    fn mode(&self) -> u32 {
        self.imp.mode as u32
    }
}

impl platform_unix::MetadataExt for crate::Metadata {
    #[inline]
    fn dev(&self) -> u64 {
        self.imp.stat.st_dev as u64
    }

    #[inline]
    fn ino(&self) -> u64 {
        self.imp.stat.st_ino
    }
}

impl fmt::Debug for DirEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DirEntry")
            .field("file_name", &self.file_name())
            .finish()
    }
}

impl fmt::Debug for FileType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self.fsobj_type {
            VDIR => "FileType::Directory",
            VREG => "FileType::Regular",
            code => return write!(f, "FileType::Invalid({})", code),
        };
        f.write_str(s)
    }
}

const VNON: u32 = 0;
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;
