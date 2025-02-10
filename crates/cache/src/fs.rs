use std::path::Path;

use cealn_fs::Cachefile;
use compio_fs::File;

use crate::platform;

/// Link existing file into cache, removing the existing file
///
/// This function is expected to handle the following cases:
///     * If the file exists, succeed (since the file is content hashed)
///     * If the path's parent directories don't exist, create them
///     * The file may already be unlinked (e.g. created with O_TMPFILE on Linux)
pub async fn link_into_cache(file: &mut Cachefile, path: &Path) -> anyhow::Result<()> {
    platform::link_into_cache(file, path).await
}

pub(crate) async fn link_into_cache_handle(file: &mut File, path: &Path) -> anyhow::Result<()> {
    platform::link_into_cache_handle(file, path).await
}

pub(crate) struct NormalizeModeResult {
    pub executable: bool,
}

pub(crate) async fn normalize_mode(file: &mut Cachefile) -> anyhow::Result<NormalizeModeResult> {
    platform::normalize_mode(file).await
}

pub(crate) async fn normalize_mode_handle(file: &mut File) -> anyhow::Result<NormalizeModeResult> {
    platform::normalize_mode_handle(file).await
}
