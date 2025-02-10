cfg_if::cfg_if! {
     if #[cfg(target_os = "linux")] {
        #[path = "linux.rs"]
        mod imp;
    } else if #[cfg(target_os = "windows")] {
        #[path = "windows.rs"]
        mod imp;
    } else if #[cfg(target_os = "macos")] {
        #[path = "macos.rs"]
        mod imp;
    } else {
        compile_error!("unsupported platform");
    }
}

pub use imp::TestFs;

use std::{
    collections::HashMap,
    io,
    path::Path,
    sync::{Arc, Mutex, Weak},
};

use tempfile::TempDir;

use cealn_core::fs::FilenameSemantics;

pub struct SharedTestFs {
    my_dir: TempDir,
    _test_fs: Arc<TestFs>,
}

impl SharedTestFs {
    pub fn new(semantics: FilenameSemantics) -> io::Result<Self> {
        let mut test_filesystems = SHARED_TEST_FS.lock().unwrap();

        let test_fs = match test_filesystems.get(&semantics).and_then(|x| x.upgrade()) {
            Some(test_fs) => test_fs,
            None => {
                let test_fs = Arc::new(TestFs::new(semantics)?);
                test_filesystems.insert(semantics, Arc::downgrade(&test_fs));
                test_fs
            }
        };

        let my_dir = TempDir::new_in(test_fs.path())?;

        Ok(SharedTestFs {
            my_dir,
            _test_fs: test_fs,
        })
    }

    pub fn path(&self) -> &Path {
        self.my_dir.path()
    }
}

lazy_static::lazy_static! {
    static ref SHARED_TEST_FS: Mutex<HashMap<FilenameSemantics, Weak<TestFs>>> = Default::default();
}
