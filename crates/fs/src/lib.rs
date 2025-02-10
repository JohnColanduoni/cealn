cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        #[path = "platform/linux.rs"]
        mod platform;
    } else if #[cfg(target_os = "macos")] {
        #[path = "platform/macos.rs"]
        mod platform;
    } else {
        compile_error!("unsupported platform");
    }
}

use std::{
    mem,
    path::{Path, PathBuf},
};

use compio_fs::File;
use tracing::error;

/// A cache-compatible file reference
///
/// This may be a product of a [`tempfile`] creation by the application, or a captured output file from another source.
/// This type contains all the information needed to ingest the file into the cache regardless of source or creation
/// method, as well as platform-specific tempfile differences (e.g. `O_TMPFILE` support on Linux).
pub struct Cachefile {
    path: Option<PathBuf>,
    needs_delete: bool,
    open_file: Option<File>,
}

impl Drop for Cachefile {
    fn drop(&mut self) {
        if self.needs_delete {
            mem::drop(self.open_file.take());
            let path = self
                .path
                .take()
                .expect("invalid to have a needs_delete file with no path");
            compio_executor::spawn(async move {
                if let Err(err) = compio_fs::remove_file(path).await {
                    error!("failed to remove cache file: {}", err);
                }
            })
        }
    }
}

#[inline]
pub async fn tempfile(directory: &Path, description: &str, executable: bool) -> anyhow::Result<Cachefile> {
    platform::tempfile(directory, description, executable).await
}

impl Cachefile {
    /// Provides the path to the cache-compatible file
    #[inline]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Provides the open file handle, if the file is already opened
    #[inline]
    pub fn open_file(&self) -> Option<&File> {
        self.open_file.as_ref()
    }

    /// Provides the open file handle, if the file is already opened
    #[inline]
    pub fn open_file_mut(&mut self) -> Option<&mut File> {
        self.open_file.as_mut()
    }

    /// Indicates if the file requires the application to delete it manually
    ///
    /// If this is `false`, that indicates some OS mechanism will cleanup the file when the process exits. This does
    /// not include the [`Cachefile`] destructor.
    #[inline]
    pub fn needs_delete(&self) -> bool {
        self.needs_delete
    }

    pub async fn ensure_open(&mut self) -> anyhow::Result<&mut File> {
        if let Some(file) = &mut self.open_file {
            Ok(file)
        } else {
            todo!()
        }
    }
}
