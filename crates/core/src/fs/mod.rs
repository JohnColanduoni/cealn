cfg_if::cfg_if! {
     if #[cfg(target_os = "linux")] {
        #[path = "linux.rs"]
        mod sys;
    } else if #[cfg(target_os = "windows")] {
        #[path = "windows.rs"]
        mod sys;
    } else if #[cfg(target_os = "macos")] {
        #[path = "macos.rs"]
        mod sys;
    } else {
        compile_error!("unsupported platfrom");
    }
}

#[cfg(unix)]
pub mod unix;

use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use ring::digest::{self, Digest};

/// Constructs a hash of a native filesystem path
pub fn hash_native_filename(algorithm: &'static digest::Algorithm, path: impl AsRef<Path>) -> Digest {
    cfg_if::cfg_if! {
        if #[cfg(target_os = "windows")] {
            use std::os::windows::prelude::*;
            let mut hasher = digest::Context::new(algorithm);
            for code_unit in path.as_ref().as_os_str().encode_wide() {
                hasher.update(&code_unit.to_be_bytes())
            }
            hasher.finish()
        } else if #[cfg(unix)] {
            use std::os::unix::prelude::*;
            digest::digest(algorithm, path.as_ref().as_os_str().as_bytes())
        } else {
            compile_error!("unsupported platform");
        }
    }
}

pub trait FileExt {
    /// Get the path of the opened file
    fn path(&self) -> io::Result<PathBuf>;

    fn file_node_identifier(&self) -> io::Result<FileNodeIdentifier>;
}

pub trait MetadataExt {
    fn file_node_identifier(&self) -> io::Result<FileNodeIdentifier>;
}

/// The handling of filenames of a filesystem, from a casing and normalization perspective
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum FilenameSemantics {
    /// Filenames are arrays of bytes, no collation
    GenericPosix,

    /// Filenames are arrays of 16-bit integers, which are usually interpreted as UTF-16 code units
    ///
    /// Under Win32 semantics, filenames are case and normalization insensitive (according to Form C)
    Ntfs { win32_semantics: bool },

    /// Filenames are are UTF-16 strings, not normalization preserving (according to Apple's Form D)
    HfsPlus { case_sensitive: bool },
    /// Filenames are are UTF-8 strings, normalization preserving but insensitive (according to Apple's Form D)
    Apfs { case_sensitive: bool },
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct FileNodeIdentifier {
    inner: self::sys::FileNodeIdentifier,
}

impl fmt::Debug for FileNodeIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}
