use std::{ffi::CString, os::unix::prelude::OsStrExt, path::Path};

use anyhow::bail;
use cealn_core::libc_call;
use cealn_fs::Cachefile;
use compio_fs::{
    os::{linux::FileExt, unix::PermissionsExt},
    File,
};

use crate::fs::NormalizeModeResult;

pub(crate) async fn link_into_cache(file: &mut Cachefile, path: &Path) -> anyhow::Result<()> {
    if let Some(_path) = file.path() {
        todo!()
    } else {
        // On Linux, this means O_TMPFILE
        let file = file.open_file_mut().unwrap();

        link_into_cache_handle(file, path).await
    }
}

pub(crate) async fn link_into_cache_handle(file: &mut File, path: &Path) -> anyhow::Result<()> {
    use compio_fs::os::linux::FileExt;

    unsafe {
        let source_path = CString::new(format!("/proc/self/fd/{}", file.as_raw_fd())).unwrap();
        let dest_path = CString::new(path.as_os_str().as_bytes())?;
        let mut have_retried = false;
        loop {
            // FIXME: use io_uring
            match libc_call!(libc::linkat(
                libc::AT_FDCWD,
                source_path.as_ptr(),
                libc::AT_FDCWD,
                dest_path.as_ptr(),
                libc::AT_SYMLINK_FOLLOW
            )) {
                Ok(_) => return Ok(()),
                Err(ref err) if err.raw_os_error() == Some(libc::EEXIST) => return Ok(()),
                Err(ref err) if err.raw_os_error() == Some(libc::ENOENT) => {
                    if have_retried {
                        bail!("failed to link into cache even after creating directories: {}", err);
                    }
                    // Directory doesn't exist, create
                    std::fs::create_dir_all(path.parent().expect("invalid cache path"))?;
                    have_retried = true;
                    continue;
                }
                Err(err) => return Err(err.into()),
            }
        }
    }
}

pub(crate) async fn normalize_mode(file: &mut Cachefile) -> anyhow::Result<NormalizeModeResult> {
    if let Some(file) = file.open_file_mut() {
        normalize_mode_handle(file).await
    } else {
        todo!()
    }
}

pub(crate) async fn normalize_mode_handle(file: &mut File) -> anyhow::Result<NormalizeModeResult> {
    let metadata = file.symlink_metadata().await?;
    let mode = metadata.permissions().mode();
    // All cache files should have mode 0o555 or 0o444 for executable and non-executable respectively
    let executable = mode & 0o100 != 0;
    let desired_mode = if executable { 0o555 } else { 0o444 };
    if mode != desired_mode {
        unsafe {
            libc_call!(libc::fchmod(file.as_raw_fd(), desired_mode as libc::mode_t))?;
        }
    }
    Ok(NormalizeModeResult { executable })
}
