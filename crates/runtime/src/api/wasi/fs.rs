use std::{
    any::Any,
    cell::{Cell, Ref, RefCell},
    collections::{hash_map, HashMap},
    fmt,
    io::{self, SeekFrom},
    ops::{BitAnd, Deref},
    sync::Arc,
};

use async_trait::async_trait;
use dashmap::DashMap;
use thiserror::Error;
use tracing::{debug, error};

use super::{super::wasi, types, Errno};

pub struct Descriptor {
    fd: types::Fd,
    preopen_path: Option<String>,

    handle: Arc<dyn Handle>,

    rights: HandleRights,
}

#[derive(Clone)]
pub struct HandleRights {
    pub(crate) base: types::Rights,
    pub(crate) inheriting: types::Rights,
}

pub struct SubSystem {
    roots: Vec<(String, Arc<dyn Handle>)>,
    // `HashMap` is fine here since we never iterate over
    fds: DashMap<types::Fd, Descriptor>,
    highest_fd: Cell<types::Fd>,
}

/// An opened file handle in the underlying filesystem
///
/// This implements the raw operations that the WASI filesystem layer requires. Note that the following is handled by
/// the layer above this interface and does not need to be implemented:
///     * WASI capability access checks
///     * Path resolution (all filenames are valid filenames, no directory separators, "." or "..")
///     * Symlink resolution
// Allow unused variables in our stub implementations
#[allow(unused_variables)]
#[async_trait]
pub trait Handle: Any + Send + Sync + fmt::Debug {
    fn file_type(&self) -> types::Filetype;

    fn rights(&self) -> HandleRights;

    async fn read(&self, iovs: &mut [io::IoSliceMut]) -> wasi::Result<usize> {
        return Err(Errno::Notcapable);
    }

    async fn write(&self, iovs: &[io::IoSlice]) -> wasi::Result<usize> {
        return Err(Errno::Notcapable);
    }

    async fn tell(&self) -> wasi::Result<types::Filesize> {
        return Err(Errno::Notcapable);
    }

    async fn seek(&self, pos: SeekFrom) -> wasi::Result<u64> {
        return Err(Errno::Notcapable);
    }

    /// Opens a direct descendant of this directory
    ///
    /// The `path_segment` is guaranteed to be a valid WASI filename (no forward slashes, not "." or ".."). Symbolic
    /// links should not be followed.
    async fn openat_child(
        &self,
        path_segment: &str,
        read: bool,
        write: bool,
        oflags: types::Oflags,
        fd_flags: types::Fdflags,
    ) -> wasi::Result<Arc<dyn Handle>> {
        return Err(Errno::Notcapable);
    }

    async fn readdir<'a>(
        &'a self,
        cookie: types::Dircookie,
    ) -> wasi::Result<Box<dyn Iterator<Item = wasi::Result<(types::Dirent, String)>> + 'a>> {
        return Err(Errno::Notcapable);
    }

    /// Reads a link contained directly in this directory
    ///
    /// The `path_segment` is guaranteed to be a valid WASI filename (no forward slashes, not "." or "..")
    async fn readlinkat_child(&self, path_segment: &str) -> wasi::Result<String> {
        return Err(Errno::Notcapable);
    }

    /// Stats this file
    ///
    /// Always stats this particular file: symlinks should never be followed.
    async fn filestat(&self) -> wasi::Result<types::Filestat> {
        return Err(Errno::Notcapable);
    }

    /// Stats a direct descendant of this directory
    ///
    /// The `path_segment` is guaranteed to be a valid WASI filename (no forward slashes, not "." or "..")
    async fn filestat_child(&self, path_segment: &str) -> wasi::Result<types::Filestat> {
        return Err(Errno::Notcapable);
    }

    fn fdstat(&self) -> wasi::Result<types::Fdflags> {
        return Err(Errno::Notcapable);
    }

    /// Checks if the given rights are provided by this file handle
    ///
    /// This should match [`Handle::get_rights`], but API implementations can instrument it for debugging.
    fn check_rights(&self, rights: &HandleRights) -> bool {
        self.rights().contains(rights)
    }
}

impl SubSystem {
    pub fn new(roots: Vec<(String, Arc<dyn Handle>)>) -> Result<Self, InitError> {
        let mut subsystem = SubSystem {
            roots,
            fds: Default::default(),
            highest_fd: Cell::new(types::Fd::from(2)),
        };

        subsystem.preopen()?;

        Ok(subsystem)
    }

    fn preopen(&mut self) -> Result<(), InitError> {
        for (path, handle) in self.roots.iter() {
            let fd = self.acquire_fd().ok_or(InitError::TooManyRoots)?;

            self.fds.insert(
                fd,
                Descriptor {
                    fd,
                    preopen_path: Some(path.to_owned()),
                    rights: handle.rights(),
                    handle: handle.to_owned(),
                },
            );
        }

        Ok(())
    }

    pub fn get_descriptor<'a>(&'a self, fd: types::Fd) -> Option<impl Deref<Target = Descriptor> + 'a> {
        self.fds.get(&fd)
    }

    pub fn new_descriptor(
        &self,
        handle: Arc<dyn Handle>,
        restricted_rights: Option<&HandleRights>,
    ) -> wasi::Result<types::Fd> {
        let fd = self.acquire_fd().ok_or(Errno::Mfile)?;

        self.fds.insert(
            fd,
            Descriptor {
                fd,
                preopen_path: None,
                rights: restricted_rights
                    .map(|rights| rights & &handle.rights())
                    .unwrap_or_else(|| handle.rights()),
                handle,
            },
        );

        Ok(fd)
    }

    fn acquire_fd(&self) -> Option<types::Fd> {
        let prev_fd = self.highest_fd.get();
        let new_fd = types::Fd::from(i32::from(prev_fd).checked_add(1)?);
        self.highest_fd.set(new_fd);
        Some(new_fd)
    }

    #[must_use]
    pub fn close(&self, fd: types::Fd) -> bool {
        self.fds.remove(&fd).is_some()
    }

    pub fn inject_fd(&self, handle: Arc<dyn Handle>, fd: Option<types::Fd>) -> Result<types::Fd, InjectFdError> {
        let fd = match fd {
            Some(fd) => fd,
            None => self.acquire_fd().ok_or(InjectFdError::TooManyFiles)?,
        };

        match self.fds.entry(fd) {
            dashmap::mapref::entry::Entry::Vacant(entry) => {
                entry.insert(Descriptor {
                    fd,
                    preopen_path: None,
                    rights: handle.rights(),
                    handle,
                });
            }
            dashmap::mapref::entry::Entry::Occupied(_) => {
                return Err(InjectFdError::AlreadyExists);
            }
        }

        Ok(fd)
    }

    pub fn dup(&self, fd: types::Fd) -> wasi::Result<types::Fd> {
        let (new_fd, new_desc) = match self.fds.get(&fd) {
            Some(entry) => {
                let new_fd = self.acquire_fd().ok_or(Errno::Mfile)?;
                let new_descriptor = Descriptor {
                    fd: new_fd,
                    preopen_path: entry.preopen_path.clone(),
                    handle: entry.handle.clone(),
                    rights: entry.rights.clone(),
                };
                (new_fd, new_descriptor)
            }
            None => return Err(Errno::Badf),
        };
        self.fds.insert(new_fd, new_desc);
        Ok(new_fd)
    }
}

const MAX_SYMLINK_EXPANSIONS: usize = 128;

impl Descriptor {
    pub fn handle(&self) -> &dyn Handle {
        &*self.handle
    }

    pub fn handle_clone(&self) -> Arc<dyn Handle> {
        self.handle.clone()
    }

    pub fn file_type(&self) -> types::Filetype {
        self.handle.file_type()
    }

    pub fn preopen_path(&self) -> Option<&str> {
        self.preopen_path.as_deref()
    }

    pub fn rights(&self) -> &HandleRights {
        &self.rights
    }

    pub fn check_rights(&self, rights: &HandleRights) -> bool {
        self.rights.contains(rights)
    }

    /// Gets the handle of the directory containing the file indicated by the relative path from this directory
    ///
    /// Handles path normalization, but following WASI conventions will not allow poping beyond the directory pointed
    /// to by this file entry.
    #[tracing::instrument(level = "trace")]
    pub async fn navigate(
        &self,
        path: &str,
        // The rights we demand of the immediate parent of the requested file
        required_container_rights: &HandleRights,
        dirflags: types::Lookupflags,
    ) -> wasi::Result<NavigateResult> {
        if path.contains('\0') {
            return Err(Errno::Ilseq);
        }

        if self.file_type() != types::Filetype::Directory {
            return Err(Errno::Notdir);
        }

        // We require each directory we move through to give us inherited rights for any rights we require
        let navigation_rights = HandleRights::new(
            types::Rights::empty(),
            required_container_rights.base | required_container_rights.inheriting,
        );
        ensure_fd_rights(self, &navigation_rights)?;

        let mut symlink_expansions = 0usize;
        let mut dir_stack = vec![self.handle_clone()];
        // TODO: this allocates a lot, we can do better
        let mut path_stack = vec![path.to_owned()];

        while let Some(current_path) = path_stack.pop() {
            if current_path.starts_with("/") {
                error!("attempted to open rooted path");
                return Err(Errno::Notcapable);
            }

            let (head, rem, trailing_slash) = match current_path.split_once('/') {
                Some((head, rem)) => (head, rem, rem.is_empty()),
                None => (&*current_path, "", false),
            };
            if !rem.is_empty() {
                path_stack.push(rem.to_owned());
            }

            // Deal with special segments
            match head {
                "" => continue,
                "." => continue,
                ".." => {
                    let _ = dir_stack.pop().ok_or_else(|| {
                        error!("attempted to open path above directory file descriptor");
                        Errno::Notcapable
                    })?;

                    if dir_stack.is_empty() {
                        error!("attempted to open path above directory file descriptor");
                        return Err(Errno::Notcapable);
                    }
                }
                _ => {}
            }

            if !path_stack.is_empty() || trailing_slash {
                let dir_handle = dir_stack.last().ok_or_else(|| {
                    error!("attempted to open path above directory file descriptor");
                    Errno::Notcapable
                })?;

                ensure_handle_rights(&**dir_handle, &navigation_rights)?;

                match dir_handle
                    .openat_child(head, false, false, types::Oflags::DIRECTORY, types::Fdflags::empty())
                    .await
                {
                    Ok(new_dir) => {
                        dir_stack.push(new_dir);
                    }
                    Err(Errno::Loop) | Err(Errno::Mlink) | Err(Errno::Notdir) => {
                        unimplemented!("handle symlink navigation")
                    }
                    Err(err) => return Err(err),
                }
            } else if dirflags.contains(types::Lookupflags::SYMLINK_FOLLOW) {
                let dir_handle = dir_stack.last().ok_or_else(|| {
                    error!("attempted to open path above directory file descriptor");
                    Errno::Notcapable
                })?;

                // FIXME: check symlink rights
                match dir_handle.readlinkat_child(head).await {
                    Ok(mut link_path) => {
                        symlink_expansions += 1;
                        if symlink_expansions > MAX_SYMLINK_EXPANSIONS {
                            return Err(Errno::Loop);
                        }

                        if trailing_slash {
                            // Preserve trailing slash in request to ensure we open the symlink as a directory
                            link_path.push('/');
                        }

                        path_stack.push(link_path);
                    }
                    Err(Errno::Inval) | Err(Errno::Noent) | Err(Errno::Notdir) => {
                        // Not a link
                        return Ok(NavigateResult::Parented {
                            parent_handle: dir_stack.pop().ok_or_else(|| {
                                error!("attempted to open path above directory file descriptor");
                                Errno::Notcapable
                            })?,
                            filename: head.to_owned(),
                        });
                    }
                    Err(err) => return Err(err),
                }
            } else {
                return Ok(NavigateResult::Parented {
                    parent_handle: dir_stack.pop().ok_or_else(|| {
                        error!("attempted to open path above directory file descriptor");
                        Errno::Notcapable
                    })?,
                    filename: head.to_owned(),
                });
            }
        }

        return Ok(NavigateResult::Unparented {
            directory_handle: dir_stack.pop().ok_or_else(|| {
                error!("attempted to open path above directory file descriptor");
                Errno::Notcapable
            })?,
        });
    }
}

impl HandleRights {
    pub fn new(base: types::Rights, inheriting: types::Rights) -> Self {
        HandleRights { base, inheriting }
    }

    pub fn from_base(base: types::Rights) -> Self {
        HandleRights {
            base,
            inheriting: types::Rights::empty(),
        }
    }

    pub fn contains(&self, rhs: &HandleRights) -> bool {
        self.base.contains(rhs.base) && self.inheriting.contains(rhs.inheriting)
    }

    /// Removes any rights that don't apply to this file type
    pub fn filter_for_type(&self, ty: types::Filetype) -> Self {
        let shared_rights =
            types::Rights::FD_FILESTAT_GET | types::Rights::FD_FILESTAT_SET_SIZE | types::Rights::FD_FILESTAT_SET_TIMES;

        // TODO: not sure I understand the permission model fully, particularly around whether stuff like PATH_RENAME_SOURCE
        // needs to be on the destination file.
        match ty {
            types::Filetype::RegularFile => HandleRights {
                base: self.base
                    & (shared_rights
                        | types::Rights::FD_DATASYNC
                        | types::Rights::FD_READ
                        | types::Rights::FD_SEEK
                        | types::Rights::FD_FDSTAT_SET_FLAGS
                        | types::Rights::FD_SYNC
                        | types::Rights::FD_TELL
                        | types::Rights::FD_WRITE
                        | types::Rights::FD_ADVISE
                        | types::Rights::FD_ALLOCATE),
                inheriting: types::Rights::empty(),
            },
            types::Filetype::Directory => HandleRights {
                base: self.base
                    & (shared_rights
                        | types::Rights::PATH_CREATE_DIRECTORY
                        | types::Rights::PATH_CREATE_FILE
                        | types::Rights::PATH_LINK_SOURCE
                        | types::Rights::PATH_LINK_TARGET
                        | types::Rights::PATH_OPEN
                        | types::Rights::FD_READDIR
                        | types::Rights::PATH_RENAME_SOURCE
                        | types::Rights::PATH_RENAME_TARGET
                        | types::Rights::PATH_FILESTAT_GET
                        | types::Rights::PATH_FILESTAT_SET_SIZE
                        | types::Rights::PATH_FILESTAT_SET_TIMES
                        | types::Rights::PATH_SYMLINK
                        | types::Rights::PATH_REMOVE_DIRECTORY
                        | types::Rights::PATH_UNLINK_FILE),
                inheriting: self.inheriting,
            },
            _ => unimplemented!(),
        }
    }
}

pub fn ensure_fd_rights(desc: &Descriptor, rights: &HandleRights) -> wasi::Result<()> {
    if !desc.check_rights(rights) {
        debug!(code = "access_check_failed", entity = "fd", actual_rights = ?desc.rights(), required_rights = ?rights);
        return Err(Errno::Notcapable);
    }

    Ok(())
}

pub fn ensure_handle_rights(handle: &dyn Handle, rights: &HandleRights) -> wasi::Result<()> {
    if !handle.check_rights(rights) {
        debug!(code = "access_check_failed", entity = "handle", actual_rights = ?handle.rights(), required_rights = ?rights);
        return Err(Errno::Notcapable);
    }

    Ok(())
}

pub enum NavigateResult {
    Parented {
        parent_handle: Arc<dyn Handle>,
        filename: String,
    },
    /// The navigation needed to navigate through the target to reach it (e.g. "somedir/.")
    Unparented { directory_handle: Arc<dyn Handle> },
}

impl<'a> BitAnd for &'a HandleRights {
    type Output = HandleRights;

    fn bitand(self, rhs: &'a HandleRights) -> HandleRights {
        HandleRights {
            base: self.base & rhs.base,
            inheriting: self.inheriting & rhs.inheriting,
        }
    }
}

#[derive(Error, Debug)]
pub enum InitError {
    #[error("ran out of file descriptors when enumerating roots")]
    TooManyRoots,
}

#[derive(Error, Debug)]
pub enum InjectFdError {
    #[error("this supplied file descriptor already exists")]
    AlreadyExists,
    #[error("ran out of file descriptors")]
    TooManyFiles,
}

impl fmt::Debug for Descriptor {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("Descriptor")
            .field("fd", &self.fd)
            .field("preopen_path", &self.preopen_path)
            .field("handle", &self.handle() as &dyn fmt::Debug)
            .finish()
    }
}

impl fmt::Debug for HandleRights {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("HandleRights")
            .field("base", &format_args!("{}", self.base))
            .field("inheriting", &format_args!("{}", self.inheriting))
            .finish()
    }
}
