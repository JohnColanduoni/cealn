use std::{
    convert::TryInto,
    fmt,
    io::{self, SeekFrom},
    sync::Arc,
};

use async_trait::async_trait;
use cealn_runtime::api::{types, Handle as ApiHandle, HandleRights, Result as WasiResult};
use cealn_source_fs::SourceFs;

pub(crate) struct NamedWorkspacesFs {
    root: Arc<Root>,
}

struct Root {
    mappings: Vec<(String, Arc<dyn ApiHandle>)>,
}

pub(crate) struct Builder {
    sourcefs_workspaces: Vec<(String, SourceFs)>,
}

impl NamedWorkspacesFs {
    pub(crate) fn builder() -> Builder {
        Builder {
            sourcefs_workspaces: Default::default(),
        }
    }

    pub fn to_handle(&self) -> Arc<dyn ApiHandle> {
        self.root.clone()
    }
}

impl Builder {
    pub(crate) fn add_source_fs(&mut self, workspace_name: String, source_fs: SourceFs) {
        self.sourcefs_workspaces.push((workspace_name, source_fs));
    }

    pub(crate) fn build(self) -> NamedWorkspacesFs {
        let mut mappings: Vec<_> = self
            .sourcefs_workspaces
            .into_iter()
            .map(|(name, fs)| (name, fs.to_handle()))
            .collect();
        // Ensure we list directory with sorted names for consistency
        mappings.sort_by(|(a, _), (b, _)| a.cmp(b));
        NamedWorkspacesFs {
            root: Arc::new(Root { mappings }),
        }
    }
}

#[async_trait]
impl ApiHandle for Root {
    fn file_type(&self) -> types::Filetype {
        types::Filetype::Directory
    }

    fn rights(&self) -> HandleRights {
        let file_rights =
            types::Rights::FD_READ | types::Rights::FD_SEEK | types::Rights::FD_TELL | types::Rights::FD_FILESTAT_GET;
        let directory_rights = types::Rights::PATH_OPEN
            | types::Rights::FD_READDIR
            | types::Rights::PATH_FILESTAT_GET
            | types::Rights::FD_FILESTAT_GET;
        HandleRights::new(directory_rights, directory_rights | file_rights)
    }

    async fn read(&self, iovs: &mut [io::IoSliceMut]) -> WasiResult<usize> {
        Err(types::Errno::Isdir)
    }

    async fn write(&self, iovs: &[io::IoSlice]) -> WasiResult<usize> {
        Err(types::Errno::Isdir)
    }

    async fn tell(&self) -> WasiResult<types::Filesize> {
        Err(types::Errno::Isdir)
    }

    async fn seek(&self, pos: SeekFrom) -> WasiResult<u64> {
        Err(types::Errno::Isdir)
    }

    async fn openat_child(
        &self,
        path_segment: &str,
        read: bool,
        write: bool,
        oflags: types::Oflags,
        fd_flags: types::Fdflags,
    ) -> WasiResult<Arc<dyn ApiHandle>> {
        // FIXME: check flags
        for (name, handle) in self.mappings.iter() {
            if name == path_segment {
                return Ok(handle.clone());
            }
        }
        Err(types::Errno::Noent)
    }

    async fn readdir<'a>(
        &'a self,
        cookie: types::Dircookie,
    ) -> WasiResult<Box<dyn Iterator<Item = WasiResult<(types::Dirent, String)>> + 'a>> {
        let iter = self
            .mappings
            .iter()
            .enumerate()
            .skip(cookie.try_into().map_err(|_| types::Errno::Overflow)?);
        let mut entries = Vec::new();
        for (index, (name, _)) in iter {
            let dirent = types::Dirent {
                // FIXME
                d_ino: 0,
                d_namlen: name.len().try_into().map_err(|_| types::Errno::Overflow)?,
                d_type: types::Filetype::Directory,
                d_next: (index + 1).try_into().map_err(|_| types::Errno::Overflow)?,
            };
            entries.push(Ok((dirent, name.clone())));
        }
        Ok(Box::new(entries.into_iter()))
    }

    async fn readlinkat_child(&self, path_segment: &str) -> WasiResult<String> {
        for (name, _) in self.mappings.iter() {
            if name == path_segment {
                // Not a link
                return Err(types::Errno::Inval);
            }
        }
        Err(types::Errno::Noent)
    }

    async fn filestat(&self) -> WasiResult<types::Filestat> {
        Ok(types::Filestat {
            // FIXME: should be unique
            dev: 0,
            ino: 0,
            filetype: types::Filetype::Directory,
            nlink: 1,
            size: 0,
            atim: 0,
            mtim: 0,
            ctim: 0,
        })
    }

    async fn filestat_child(&self, path_segment: &str) -> WasiResult<types::Filestat> {
        for (name, _) in self.mappings.iter() {
            if name == path_segment {
                return Ok(types::Filestat {
                    // FIXME: should be unique
                    dev: 0,
                    ino: 0,
                    filetype: types::Filetype::Directory,
                    nlink: 1,
                    size: 0,
                    atim: 0,
                    mtim: 0,
                    ctim: 0,
                });
            }
        }
        Err(types::Errno::Noent)
    }

    fn fdstat(&self) -> WasiResult<types::Fdflags> {
        Ok(types::Fdflags::empty())
    }
}

impl fmt::Debug for Root {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NamedWorkspaceFsRoot").finish()
    }
}
