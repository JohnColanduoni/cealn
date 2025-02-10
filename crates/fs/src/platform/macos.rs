use std::path::Path;

use compio_fs::{os::macos::OpenOptionsExt, OpenOptions};
use rand::{distributions::Alphanumeric, Rng};

use crate::Cachefile;

const RAND_CHARS_LEN: usize = 8;

pub(crate) async fn tempfile(directory: &Path, description: &str, executable: bool) -> anyhow::Result<Cachefile> {
    let mode = if executable { 0o555 } else { 0o444 };
    let random_chars = {
        let mut rng = rand::thread_rng();
        let mut random_chars: [u8; RAND_CHARS_LEN] = [0u8; RAND_CHARS_LEN];
        for c_dest in random_chars.iter_mut() {
            *c_dest = rng.sample(Alphanumeric);
        }
        random_chars
    };
    let filename = directory.join(format!(
        "{}-{}",
        description,
        std::str::from_utf8(&random_chars[..]).unwrap()
    ));
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(mode)
        .open(&filename)
        .await?;
    Ok(Cachefile {
        path: Some(filename),
        needs_delete: true,
        open_file: Some(file),
    })
}
