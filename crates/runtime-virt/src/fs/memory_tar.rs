use std::{
    borrow::Cow,
    cmp,
    collections::{btree_map, BTreeMap},
    convert::TryFrom,
    fmt, io,
    io::SeekFrom,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use thiserror::Error;
use tracing::trace;

use cealn_runtime::{
    api::{types, types::Errno, Handle as ApiHandle, HandleRights, Result},
    interpreter::StaticHandle as ApiStaticHandle,
};

pub struct InMemoryTar(Arc<_InMemoryTar>);

struct _InMemoryTar {
    // TODO: Better structure (LOUDS?)
    root: InMemoryDirectory,
    bytes: Cow<'static, [u8]>,
}

struct InMemoryDirectory {
    inode: u64,
    children: BTreeMap<String, InMemoryDirEntry>,
    // Indicates whether this directory is explicitly present in this tree or was only implicitly created because
    // a file or directory inside of it was found.
    //
    // Used to determine exposed roots of the filesystem.
    explicit: bool,
}

struct File {
    inode: u64,
    offset: u64,
    len: u64,
}

enum InMemoryDirEntry {
    Directory(InMemoryDirectory),
    File(File),
}

struct Handle {
    filesystem: Arc<_InMemoryTar>,
    // Normalized path of file from root, not including trailing slash (even if directory)
    path: String,

    current_offset: Mutex<u64>,
}

struct StaticHandle {
    filesystem: Arc<_InMemoryTar>,
    // Normalized path of file from root, not including trailing slash (even if directory)
    path: String,
}

impl InMemoryTar {
    #[tracing::instrument("InMemoryTar::build", level = "debug", skip(bytes) err)]
    pub fn build(bytes: Cow<'static, [u8]>) -> std::result::Result<Self, BuildError> {
        let bytes_slice = &*bytes;

        let mut next_inode: u64 = 0;
        let mut root = InMemoryDirectory {
            inode: next_inode,
            explicit: false,
            children: Default::default(),
        };
        next_inode += 1;

        let mut reader = tar::Archive::new(bytes_slice);

        for entry in reader.entries()? {
            let entry = entry?;

            trace!(code = "processing_entry", header = ?entry.header());

            let path = entry.path_bytes();

            let mut parent_node: &mut InMemoryDirectory = &mut root;
            let mut segment_iter = path
                .split(|&x| x == b'/')
                .filter(|&x| !x.is_empty() && x != b".")
                .peekable();
            while let Some(segment) = segment_iter.next() {
                let segment = std::str::from_utf8(segment).map_err(BuildError::InvalidUtf8Filename)?;

                if segment == ".." {
                    return Err(BuildError::UnexpectedParentEntry);
                }

                if segment_iter.peek().is_some() {
                    // Nested directory
                    match parent_node.children.entry(segment.to_owned()).or_insert_with(|| {
                        let inode = next_inode;
                        next_inode += 1;
                        InMemoryDirEntry::Directory(InMemoryDirectory {
                            inode,
                            explicit: false,
                            children: Default::default(),
                        })
                    }) {
                        InMemoryDirEntry::Directory(directory) => {
                            parent_node = directory;
                        }
                        InMemoryDirEntry::File { .. } => return Err(BuildError::DuplicateEntry),
                    }
                } else {
                    // Full entry
                    match parent_node.children.entry(segment.to_owned()) {
                        btree_map::Entry::Occupied(mut dest_entry) => {
                            // In general this is an error, but in the case that the current entry is an implicit
                            // directory it is acceptable and we just flip the entry to explicit.
                            match (entry.header().entry_type(), dest_entry.get_mut()) {
                                (tar::EntryType::Directory, InMemoryDirEntry::Directory(dir)) if !dir.explicit => {
                                    dir.explicit = true;
                                }
                                _ => return Err(BuildError::DuplicateEntry),
                            }
                        }
                        btree_map::Entry::Vacant(dest_entry) => {
                            let inode = next_inode;
                            next_inode += 1;
                            match entry.header().entry_type() {
                                tar::EntryType::Regular => {
                                    dest_entry.insert(InMemoryDirEntry::File(File {
                                        inode,
                                        offset: entry.raw_file_position(),
                                        len: entry.header().size()?,
                                    }));
                                }
                                tar::EntryType::Directory => {
                                    dest_entry.insert(InMemoryDirEntry::Directory(InMemoryDirectory {
                                        inode,
                                        children: Default::default(),
                                        explicit: true,
                                    }));
                                }
                                other => return Err(BuildError::UnsupportedEntryType(other)),
                            }
                        }
                    }
                    break;
                }
            }
        }

        Ok(InMemoryTar(Arc::new(_InMemoryTar { bytes, root })))
    }

    pub fn roots(&self) -> Vec<(String, Arc<dyn ApiStaticHandle>)> {
        let mut accum = Vec::new();
        self.0.root.get_roots("", &mut accum);

        accum
            .into_iter()
            .map(|(_, path)| {
                (
                    path.clone(),
                    Arc::new(StaticHandle {
                        filesystem: self.0.clone(),
                        path,
                    }) as Arc<dyn ApiStaticHandle>,
                )
            })
            .collect()
    }
}

impl InMemoryDirEntry {
    fn inode(&self) -> u64 {
        match self {
            InMemoryDirEntry::Directory(dir) => dir.inode,
            InMemoryDirEntry::File(f) => f.inode,
        }
    }

    fn file_type(&self) -> types::Filetype {
        match self {
            InMemoryDirEntry::Directory(_) => types::Filetype::Directory,
            InMemoryDirEntry::File(_) => types::Filetype::RegularFile,
        }
    }
}

impl InMemoryDirectory {
    fn get_roots(&self, prefix: &str, accum: &mut Vec<(types::Filetype, String)>) {
        // Walk directory tree until we hit a file or explicit directory
        if self.explicit {
            trace!(code = "found_root_dir", path = prefix);
            accum.push((types::Filetype::Directory, prefix.to_owned()));
        } else {
            let mut child_prefix = prefix.to_owned();
            for (name, entry) in self.children.iter() {
                child_prefix.truncate(prefix.len());
                child_prefix.push('/');
                child_prefix.push_str(name);
                match entry {
                    InMemoryDirEntry::Directory(d) => {
                        d.get_roots(&child_prefix, accum);
                    }
                    InMemoryDirEntry::File { .. } => {
                        trace!(code = "found_root_file", path = &*child_prefix);
                        accum.push((types::Filetype::RegularFile, child_prefix.clone()));
                    }
                }
            }
        }
    }
}

impl File {
    fn as_bytes<'a, 'b>(&'a self, filesystem: &'b _InMemoryTar) -> &'b [u8] {
        let file_start = usize::try_from(self.offset).unwrap();
        let file_end = file_start.checked_add(usize::try_from(self.len).unwrap()).unwrap();
        &filesystem.bytes[file_start..file_end]
    }
}

// Separate type for lookup result used to deal uniformly with the possibility that one is lookup up the root directory
enum LookupResult<'a> {
    File(&'a File),
    Directory(&'a InMemoryDirectory),
}

impl Handle {
    // Finds an entry relative to this directory. Assumes that subpath is relative and already normalized without a
    // trailing slash
    fn lookup<'a>(&'a self, subpath: &str) -> Option<LookupResult<'a>> {
        assert!(!subpath.starts_with("/") && !subpath.ends_with("/"));

        let mut current_dir = &self.filesystem.root;

        let mut segments_iter = self.path[1..]
            .split('/')
            .chain(subpath.split('/'))
            // Can happen if `self.path` is "/" or `subpath` is ""
            .filter(|x| !x.is_empty())
            .peekable();
        while let Some(segment) = segments_iter.next() {
            match current_dir.children.get(segment) {
                Some(InMemoryDirEntry::Directory(dir)) => {
                    current_dir = dir;
                }
                Some(InMemoryDirEntry::File(file)) => {
                    if segments_iter.peek().is_some() {
                        return None;
                    } else {
                        return Some(LookupResult::File(file));
                    }
                }
                None => return None,
            }
        }

        // If we got here, we hit the end of the loop without missing any entries
        Some(LookupResult::Directory(current_dir))
    }

    fn lookup_self(&self) -> LookupResult {
        self.lookup("").expect("invalid handle was allowed to be crated")
    }
}

#[async_trait]
impl ApiHandle for Handle {
    fn file_type(&self) -> types::Filetype {
        match self.lookup_self() {
            LookupResult::Directory(_) => types::Filetype::Directory,
            LookupResult::File(_) => types::Filetype::RegularFile,
        }
    }

    fn rights(&self) -> HandleRights {
        let file_rights =
            types::Rights::FD_READ | types::Rights::FD_SEEK | types::Rights::FD_TELL | types::Rights::FD_FILESTAT_GET;
        let directory_rights = types::Rights::PATH_OPEN
            | types::Rights::FD_READDIR
            | types::Rights::PATH_FILESTAT_GET
            | types::Rights::FD_FILESTAT_GET;

        // TODO: for some reason,
        match self.lookup_self() {
            LookupResult::Directory(_) => HandleRights::new(directory_rights, directory_rights | file_rights),
            LookupResult::File(_) => HandleRights::from_base(file_rights),
        }
    }

    async fn read(&self, iovs: &mut [io::IoSliceMut]) -> Result<usize> {
        let file_slice = match self.lookup_self() {
            LookupResult::File(file) => file.as_bytes(&self.filesystem),
            LookupResult::Directory(_) => return Err(Errno::Isdir),
        };

        let mut current_offset = self.current_offset.lock().unwrap();
        let mut src_slice = file_slice
            .get(usize::try_from(*current_offset).map_err(|_| Errno::Inval)?..)
            .ok_or(Errno::Inval)?;

        let mut bytes_read = 0usize;
        for vector in iovs {
            if src_slice.is_empty() {
                break;
            }

            let slice_bytes = cmp::min(vector.len(), src_slice.len());

            vector[..slice_bytes].copy_from_slice(&src_slice[..slice_bytes]);
            src_slice = &src_slice[slice_bytes..];

            bytes_read += slice_bytes;
        }

        *current_offset += bytes_read as u64;

        Ok(bytes_read)
    }

    async fn tell(&self) -> Result<types::Filesize> {
        if self.file_type() != types::Filetype::RegularFile {
            return Err(Errno::Isdir);
        }

        Ok(*self.current_offset.lock().unwrap())
    }

    async fn seek(&self, pos: SeekFrom) -> Result<u64> {
        let file = match self.lookup_self() {
            LookupResult::File(file) => file,
            _ => return Err(Errno::Isdir),
        };

        let mut current_offset = self.current_offset.lock().unwrap();

        let absolute_offset = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::Current(offset) => safe_offset_add(*current_offset, offset).ok_or(Errno::Overflow)?,
            SeekFrom::End(offset) => safe_offset_add(file.len, offset).ok_or(Errno::Overflow)?,
        };

        if absolute_offset > file.len {
            return Err(Errno::Inval);
        }

        *current_offset = absolute_offset;

        Ok(absolute_offset)
    }

    async fn openat_child(
        &self,
        path_segment: &str,
        _read: bool,
        write: bool,
        oflags: types::Oflags,
        _fd_flags: types::Fdflags,
    ) -> Result<Arc<dyn ApiHandle>> {
        let directory = match self.lookup_self() {
            LookupResult::Directory(dir) => dir,
            _ => return Err(Errno::Notdir),
        };

        if write {
            return Err(Errno::Notcapable);
        }

        if oflags.contains(types::Oflags::TRUNC) {
            return Err(Errno::Notcapable);
        }

        let entry = directory.children.get(path_segment).ok_or(Errno::Noent)?;

        if oflags.contains(types::Oflags::DIRECTORY) {
            match entry {
                InMemoryDirEntry::Directory(_) => {}
                _ => return Err(Errno::Notdir),
            }
        }

        Ok(Arc::new(Handle {
            filesystem: self.filesystem.clone(),
            path: format!("{}/{}", self.path, path_segment),
            current_offset: Mutex::new(0),
        }))
    }

    async fn readdir<'a>(
        &'a self,
        cookie: types::Dircookie,
    ) -> Result<Box<dyn Iterator<Item = Result<(types::Dirent, String)>> + 'a>> {
        let directory = match self.lookup_self() {
            LookupResult::Directory(dir) => dir,
            _ => return Err(Errno::Notdir),
        };

        Ok(Box::new(
            directory
                .children
                .iter()
                .skip(usize::try_from(cookie).map_err(|_| Errno::Inval)?)
                .enumerate()
                .map(move |(from_cookie_index, (name, entry))| {
                    Ok((
                        types::Dirent {
                            d_next: cookie
                                .checked_add(from_cookie_index as u64)
                                .ok_or(Errno::Overflow)?
                                .checked_add(1)
                                .ok_or(Errno::Overflow)?,
                            d_ino: entry.inode(),
                            d_namlen: types::Dirnamlen::try_from(name.len())?,
                            d_type: entry.file_type(),
                        },
                        name.to_owned(),
                    ))
                }),
        ))
    }

    async fn readlinkat_child(&self, _path_segment: &str) -> Result<String> {
        Err(Errno::Inval)
    }

    async fn filestat(&self) -> Result<types::Filestat> {
        self.lookup_self().filestat()
    }

    /// Stats a direct descendant of this directory
    ///
    /// The `path_segment` is guaranteed to be a valid WASI filename (no forward slashes, not "." or "..")
    async fn filestat_child(&self, path_segment: &str) -> Result<types::Filestat> {
        self.lookup(path_segment).ok_or(Errno::Noent)?.filestat()
    }

    fn fdstat(&self) -> Result<types::Fdflags> {
        Ok(types::Fdflags::empty())
    }
}

impl ApiStaticHandle for StaticHandle {
    fn instantiate(&self) -> Arc<dyn ApiHandle> {
        Arc::new(Handle {
            filesystem: self.filesystem.clone(),
            path: self.path.clone(),
            current_offset: Mutex::new(0),
        })
    }
}

impl LookupResult<'_> {
    fn filestat(&self) -> Result<types::Filestat> {
        match self {
            LookupResult::Directory(dir) => {
                Ok(types::Filestat {
                    // FIXME: Device ID needs to be unique accross filesystems
                    dev: 0,
                    ino: dir.inode,
                    filetype: types::Filetype::Directory,
                    nlink: 1,
                    size: 0,
                    atim: 0,
                    mtim: 0,
                    ctim: 0,
                })
            }
            LookupResult::File(file) => {
                Ok(types::Filestat {
                    // FIXME: Device ID needs to be unique accross filesystems
                    dev: 0,
                    ino: file.inode,
                    filetype: types::Filetype::RegularFile,
                    nlink: 1,
                    size: file.len,
                    atim: 0,
                    mtim: 0,
                    ctim: 0,
                })
            }
        }
    }
}

fn safe_offset_add(from: u64, offset: i64) -> Option<u64> {
    match offset {
        x if x > 0 => from.checked_add(u64::try_from(offset).unwrap()),
        i64::MIN => from.checked_add(1u64 << 63),
        x => from.checked_add(u64::try_from(-x).unwrap()),
    }
}

#[derive(Error, Debug)]
pub enum BuildError {
    #[error("data is not a valid tar archive: {0}")]
    InvalidTar(#[from] io::Error),
    #[error("encountered a '..' in a path inside the archive")]
    UnexpectedParentEntry,
    #[error("encountered a duplicate file or directory")]
    DuplicateEntry,
    #[error("only regular files and directories are supported, but encountered {0:?}")]
    UnsupportedEntryType(tar::EntryType),
    #[error("encountered a file with a name that is not valid utf-8")]
    InvalidUtf8Filename(std::str::Utf8Error),
}

impl fmt::Debug for Handle {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.debug_struct("memory_tar::Handle").field("path", &self.path).finish()
    }
}
