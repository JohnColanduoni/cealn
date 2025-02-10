use std::{io::Result, time::SystemTime};

use crate::platform;

pub struct Metadata {
    pub(crate) imp: platform::Metadata,
}

pub struct Permissions {
    pub(crate) imp: platform::Permissions,
}

impl Metadata {
    #[inline]
    pub fn permissions(&self) -> Permissions {
        let imp = self.imp.permissions();
        Permissions { imp }
    }

    #[inline]
    pub fn is_dir(&self) -> bool {
        self.imp.is_dir()
    }

    #[inline]
    pub fn is_file(&self) -> bool {
        self.imp.is_file()
    }

    #[inline]
    pub fn is_symlink(&self) -> bool {
        self.imp.is_symlink()
    }

    #[inline]
    pub fn modified(&self) -> Result<SystemTime> {
        self.imp.modified()
    }

    #[inline]
    pub fn len(&self) -> u64 {
        self.imp.len()
    }
}
