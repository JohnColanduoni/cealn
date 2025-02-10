use std::{io::Result, path::Path};

use crate::{platform, File};

pub struct OpenOptions {
    pub(crate) read: bool,
    pub(crate) write: bool,
    pub(crate) create: bool,
    pub(crate) create_new: bool,
    pub(crate) append: bool,
    pub(crate) truncate: bool,
    pub(crate) imp: platform::OpenOptions,
}

impl OpenOptions {
    pub fn new() -> OpenOptions {
        OpenOptions {
            read: false,
            write: false,
            create: false,
            create_new: false,
            append: false,
            truncate: false,
            imp: platform::OpenOptions::new(),
        }
    }

    pub fn read(&mut self, read: bool) -> &mut Self {
        self.read = read;
        self
    }

    pub fn write(&mut self, write: bool) -> &mut Self {
        self.write = write;
        self
    }

    pub fn create(&mut self, create: bool) -> &mut Self {
        self.create = create;
        self
    }

    pub fn create_new(&mut self, create_new: bool) -> &mut Self {
        self.create_new = create_new;
        self
    }

    pub fn append(&mut self, append: bool) -> &mut Self {
        self.append = append;
        self
    }

    pub fn truncate(&mut self, truncate: bool) -> &mut Self {
        self.truncate = truncate;
        self
    }

    pub async fn open(&self, path: impl AsRef<Path>) -> Result<File> {
        let imp = platform::File::open_with_options(self, path.as_ref()).await?;
        Ok(File { imp })
    }
}
