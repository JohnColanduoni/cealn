use std::path::Path;

use compio_fs::{os::linux::OpenOptionsExt, OpenOptions};

use crate::Cachefile;

pub(crate) async fn tempfile(directory: &Path, _description: &str, executable: bool) -> anyhow::Result<Cachefile> {
    unsafe {
        let mode = if executable { 0o555 } else { 0o444 };
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .tmpfile(true)
            .mode(mode)
            .open(directory)
            .await?;
        Ok(Cachefile {
            path: None,
            needs_delete: false,
            open_file: Some(file),
        })
    }
}
