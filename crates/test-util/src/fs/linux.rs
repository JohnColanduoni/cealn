use std::{io, path::Path};

use tempfile::TempDir;

use cealn_core::fs::FilenameSemantics;

pub struct TestFs {
    path: TempDir,
}

impl TestFs {
    pub fn new(semantics: FilenameSemantics) -> io::Result<TestFs> {
        match semantics {
            FilenameSemantics::GenericPosix => {}
            _ => panic!("unsupported filename semantics on this platform"),
        }

        let path = TempDir::new()?;

        Ok(TestFs { path })
    }

    pub fn path(&self) -> &Path {
        self.path.path()
    }
}
