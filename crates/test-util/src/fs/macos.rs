use std::{
    io,
    path::{Path, PathBuf},
    process::Command,
};

use tempfile::{NamedTempFile, TempDir};

use cealn_core::fs::FilenameSemantics;

pub struct TestFs {
    _disk_image: NamedTempFile,
    mounted_path: TempDir,
}

impl Drop for TestFs {
    fn drop(&mut self) {
        let mut unmount_command = Command::new("hdiutil");

        unmount_command
            .arg("detach")
            .arg("-quiet")
            .arg(self.mounted_path.path())
            .arg("-force");

        match unmount_command.status() {
            Ok(ref status) if status.success() => {}
            Ok(status) => {
                eprintln!(
                    "failed to unmount image {:?} at {:?}: exit status {:?}",
                    self._disk_image.path(),
                    self.mounted_path.path(),
                    status
                );
            }
            Err(err) => {
                eprintln!(
                    "failed to unmount image {:?} at {:?}: exit status {:?}",
                    self._disk_image.path(),
                    self.mounted_path.path(),
                    err
                );
            }
        }
    }
}

impl TestFs {
    pub fn new(semantics: FilenameSemantics) -> io::Result<TestFs> {
        let disk_image = tempfile::Builder::new().suffix(".sparseimage").tempfile()?;

        let mut create_command = Command::new("hdiutil");

        create_command
            .arg("create")
            .arg("-type")
            .arg("SPARSE")
            .arg("-size")
            .arg("64m")
            .arg("-ov")
            .arg("-quiet");

        match semantics {
            FilenameSemantics::HfsPlus { case_sensitive: true } => {
                create_command.arg("-fs").arg("Case-sensitive Journaled HFS+");
            }
            FilenameSemantics::HfsPlus { case_sensitive: false } => {
                create_command.arg("-fs").arg("Journaled HFS+");
            }
            FilenameSemantics::Apfs { case_sensitive: true } => {
                create_command.arg("-fs").arg("Case-sensitive APFS");
            }
            FilenameSemantics::Apfs { case_sensitive: false } => {
                create_command.arg("-fs").arg("APFS");
            }
            _ => panic!("unsupported filename semantics on this platform"),
        }

        create_command.arg(disk_image.path());

        let result = create_command.status()?;
        if !result.success() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("hdiutil create failed with status {:?}", result),
            ));
        }

        let mounted_path = TempDir::new()?;
        let mut mount_command = Command::new("hdiutil");

        mount_command
            .arg("attach")
            .arg("-quiet")
            .arg("-mountpoint")
            .arg(mounted_path.path())
            .arg(disk_image.path());

        let result = mount_command.status()?;
        if !result.success() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                format!("hdiutil attach failed with status {:?}", result),
            ));
        }

        Ok(TestFs {
            _disk_image: disk_image,
            mounted_path,
        })
    }

    pub fn path(&self) -> &Path {
        self.mounted_path.path()
    }
}
