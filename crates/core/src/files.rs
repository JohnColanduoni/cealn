use std::{
    fs::{self, File, OpenOptions},
    io,
    path::Path,
};

use thiserror::Error;
use tracing::error;

use crate::{fs::FileExt, trace_call_result, tracing::error_value};

const WORKSPACE_FILENAME: &str = "workspace.cealn";

#[derive(Error, Debug)]
pub enum WellKnownFileError {
    #[error("file does not exist")]
    DoesNotExist,
    #[error("file exists, but with a different casing")]
    ExistsWithDifferentCase,
}

pub type WellKnownFileResult<T> = ::std::result::Result<T, WellKnownFileError>;

/// Determines whether a workspace file exists in the given directory
///
/// We require the workspace file to be lower cased even on case insenstive filesystems. This function handles that
/// accurately.
pub fn workspace_file_exists_in(directory: impl AsRef<Path>) -> io::Result<WellKnownFileResult<()>> {
    file_exists_with_case(directory.as_ref(), WORKSPACE_FILENAME.as_ref())
}

pub fn open_workspace_file_in(
    directory: impl AsRef<Path>,
    options: &OpenOptions,
) -> io::Result<WellKnownFileResult<File>> {
    open_file_with_case(directory.as_ref(), WORKSPACE_FILENAME.as_ref(), options)
}

fn file_exists_with_case(directory: &Path, filename: &Path) -> io::Result<WellKnownFileResult<()>> {
    if !directory.join(filename).exists() {
        return Ok(Err(WellKnownFileError::DoesNotExist));
    }

    // Rust doesn't have an easy way to get the case preserved filename without reading the whole directory. If this
    // becomes an issue we can work on that with the native APIs.
    for entry in trace_call_result!(fs::read_dir(directory))? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(err) => {
                error!(error = error_value(&err), "fs::read_dir failed: {}", err);
                return Err(err);
            }
        };
        if entry
            .path()
            .file_name()
            .and_then(|f| f.to_str())
            .map(|f| f == WORKSPACE_FILENAME)
            == Some(true)
        {
            return Ok(Ok(()));
        }
    }

    return Ok(Err(WellKnownFileError::ExistsWithDifferentCase));
}

fn open_file_with_case(
    directory: &Path,
    filename: &Path,
    options: &OpenOptions,
) -> io::Result<WellKnownFileResult<File>> {
    let file = match options.open(directory.join(filename)) {
        Ok(file) => file,
        Err(ref err) if err.kind() == io::ErrorKind::NotFound => return Ok(Err(WellKnownFileError::DoesNotExist)),
        Err(err) => return Err(err),
    };

    let actual_path = file.path()?;
    if actual_path.file_name().unwrap() != filename {
        return Ok(Err(WellKnownFileError::ExistsWithDifferentCase));
    }

    Ok(Ok(file))
}
