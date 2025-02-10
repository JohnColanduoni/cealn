use std::{
    borrow::Cow,
    ffi::OsStr,
    fmt,
    io::Result,
    path::{Path, PathBuf},
    pin::Pin,
    task::{Context, Poll},
};

use futures::prelude::*;
use pin_project::pin_project;

use crate::{platform, File, Metadata, OpenOptions};

pub struct Directory {
    pub(crate) imp: platform::Directory,
}

#[pin_project]
pub struct ReadDir<'a> {
    directory: &'a mut Directory,
    #[pin]
    pub(crate) imp: platform::ReadDir,
}

pub struct DirEntry {
    pub(crate) imp: platform::DirEntry,
}

pub struct FileType {
    pub(crate) imp: platform::FileType,
}

#[derive(Clone, Debug)]
pub struct DirectoryOpenOptions {
    create: bool,
    create_new: bool,
}

impl Directory {
    #[inline]
    pub async fn open(path: impl AsRef<Path>) -> Result<Directory> {
        let imp = platform::Directory::open(path.as_ref()).await?;
        Ok(Directory { imp })
    }

    #[inline]
    pub async fn create(path: impl AsRef<Path>) -> Result<()> {
        platform::Directory::create(path.as_ref()).await
    }

    #[inline]
    pub async fn create_all(path: impl AsRef<Path>) -> Result<()> {
        platform::Directory::create_all(path.as_ref()).await
    }

    #[inline]
    pub async fn open_at_file(&mut self, path: impl AsRef<Path>) -> Result<File> {
        let imp = self
            .imp
            .open_at_file_with_options(OpenOptions::new().read(true), path.as_ref())
            .await?;
        Ok(File { imp })
    }

    #[inline]
    pub async fn open_at_file_with_options(&mut self, options: &OpenOptions, path: impl AsRef<Path>) -> Result<File> {
        let imp = self.imp.open_at_file_with_options(options, path.as_ref()).await?;
        Ok(File { imp })
    }

    #[inline]
    pub async fn open_at_directory(&mut self, path: impl AsRef<Path>) -> Result<Directory> {
        let imp = self.imp.open_at_directory(path.as_ref()).await?;
        Ok(Directory { imp })
    }

    #[inline]
    pub async fn create_at_directory(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.imp.create_at_directory(path.as_ref()).await
    }

    #[inline]
    pub async fn create_at_directory_all(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.imp.create_at_directory_all(path.as_ref()).await
    }

    #[inline]
    pub async fn link_at(&mut self, link_path: impl AsRef<Path>, target_path: impl AsRef<Path>) -> Result<()> {
        self.imp.link_at(link_path.as_ref(), target_path.as_ref()).await
    }

    #[inline]
    pub async fn symlink_at(&mut self, link_path: impl AsRef<Path>, target_path: impl AsRef<Path>) -> Result<()> {
        self.imp.symlink_at(link_path.as_ref(), target_path.as_ref()).await
    }

    #[inline]
    pub async fn unlink_at(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.imp.unlink_at(path.as_ref()).await
    }

    #[inline]
    pub async fn read_link_at(&mut self, link_path: impl AsRef<Path>) -> Result<PathBuf> {
        self.imp.read_link_at(link_path.as_ref()).await
    }

    #[inline]
    pub async fn remove_dir_at(&mut self, path: impl AsRef<Path>) -> Result<()> {
        self.imp.remove_dir_at(path.as_ref()).await
    }

    #[inline]
    pub async fn read_dir(&mut self) -> Result<ReadDir> {
        let imp = self.imp.read_dir().await?;
        Ok(ReadDir { directory: self, imp })
    }

    #[inline]
    pub async fn symlink_metadata_at(&mut self, path: impl AsRef<Path>) -> Result<Metadata> {
        let imp = self.imp.symlink_metadata_at(path.as_ref()).await?;
        Ok(Metadata { imp })
    }

    #[inline]
    pub async fn remove_all(&mut self) -> Result<()> {
        platform::Directory::remove_all(self).await
    }

    #[inline]
    pub fn clone(&self) -> Result<Directory> {
        let imp = self.imp.clone()?;
        Ok(Directory { imp })
    }
}

pub async fn remove_dir(path: impl AsRef<Path>) -> Result<()> {
    platform::remove_dir(path.as_ref()).await
}

pub async fn remove_dir_all(path: impl AsRef<Path>) -> Result<()> {
    let mut directory = Directory::open(path.as_ref()).await?;
    directory.remove_all().await?;
    platform::remove_dir(path.as_ref()).await
}

impl DirEntry {
    #[inline]
    pub fn file_name(&self) -> Cow<OsStr> {
        self.imp.file_name()
    }

    #[inline]
    pub async fn file_type(&self) -> Result<FileType> {
        let imp = self.imp.file_type().await?;
        Ok(FileType { imp })
    }
}

impl FileType {
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
}

impl<'a> Stream for ReadDir<'a> {
    type Item = Result<DirEntry>;

    #[inline]
    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        this.imp.poll_next(cx).map(|x| x.map(|x| x.map(|imp| DirEntry { imp })))
    }
}

impl fmt::Debug for DirEntry {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.imp, f)
    }
}

impl fmt::Debug for FileType {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.imp, f)
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, File as StdFile},
        path::PathBuf,
    };

    use compio_executor::LocalPool;
    use compio_internal_util::assert_func_send;
    use static_assertions::assert_impl_all;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn open_dir() {
        let tempdir = TempDir::new().unwrap();

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let _directory = Directory::open(tempdir.path()).await.unwrap();
        });
    }

    #[test]
    fn create_dir() {
        let tempdir = TempDir::new().unwrap();
        let subdir_path = tempdir.path().join("somedir");

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            Directory::create(&subdir_path).await.unwrap();

            assert!(subdir_path.is_dir());
        });
    }

    #[test]
    fn create_all() {
        let tempdir = TempDir::new().unwrap();
        let subdir_path = tempdir.path().join("somedir/somemoredir");

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            Directory::create_all(&subdir_path).await.unwrap();

            assert!(subdir_path.is_dir());
        });
    }

    #[test]
    fn open_at_file() {
        let tempdir = TempDir::new().unwrap();
        let file_path = tempdir.path().join("somefile.txt");
        {
            let _std_file = StdFile::create(&file_path).unwrap();
        }

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let mut directory = Directory::open(tempdir.path()).await.unwrap();
            let _file = directory.open_at_file(&file_path).await.unwrap();
        });
    }

    #[test]
    fn open_at_directory() {
        let tempdir = TempDir::new().unwrap();
        let subdir_path = tempdir.path().join("somedir");
        fs::create_dir_all(&subdir_path).unwrap();

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let mut directory = Directory::open(tempdir.path()).await.unwrap();
            let _subdir = directory.open_at_directory(&subdir_path).await.unwrap();
        });
    }

    #[test]
    fn read_dir() {
        let tempdir = TempDir::new().unwrap();
        let mut file_paths = Vec::new();
        let mut subdir_paths = Vec::new();
        for i in 0..128 {
            let file_path = tempdir.path().join(format!("somefile{}.txt", i));
            let _file = StdFile::create(&file_path).unwrap();
            file_paths.push(file_path);
        }
        for i in 0..128 {
            let subdir_path = tempdir.path().join(format!("somedir{}", i));
            fs::create_dir_all(&subdir_path).unwrap();
            subdir_paths.push(subdir_path);
        }

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let mut directory = Directory::open(tempdir.path()).await.unwrap();
            let mut read_dir = directory.read_dir().await.unwrap();

            while let Some(entry) = read_dir.try_next().await.unwrap() {
                let file_type = entry.file_type().await.unwrap();
                if file_type.is_dir() {
                    let (i, _) = subdir_paths
                        .iter()
                        .enumerate()
                        .find(|(i, path)| path.file_name().unwrap() == &*entry.file_name())
                        .expect("didn't find expected directory");
                    subdir_paths.remove(i);
                } else if file_type.is_file() {
                    let (i, _) = file_paths
                        .iter()
                        .enumerate()
                        .find(|(i, path)| path.file_name().unwrap() == &*entry.file_name())
                        .expect("didn't find expected file");
                    file_paths.remove(i);
                } else {
                    panic!("unexpected file type {:?}", file_type)
                }
            }
        });
    }

    #[test]
    fn remove_all() {
        let tempdir = TempDir::new().unwrap();
        for i in 0..128 {
            let file_path = tempdir.path().join(format!("somefile{}.txt", i));
            let _file = StdFile::create(&file_path).unwrap();
        }
        for i in 0..10 {
            let subdir_path = tempdir.path().join(format!("somedir{}", i));
            fs::create_dir_all(&subdir_path).unwrap();

            for i in 0..5 {
                let sub_subdir_path = subdir_path.join(format!("somesubdir{}", i));
                fs::create_dir_all(&sub_subdir_path).unwrap();

                for i in 0..5 {
                    let sub_sub_subdir_path = sub_subdir_path.join(format!("somesubsubdir{}", i));
                    fs::create_dir_all(&sub_subdir_path).unwrap();
                }

                for i in 0..5 {
                    let file_path = sub_subdir_path.join(format!("somefile{}.txt", i));
                    let _file = StdFile::create(&file_path).unwrap();
                }
            }
        }

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let mut directory = Directory::open(tempdir.path()).await.unwrap();
            directory.remove_all().await.unwrap();
        });

        let list = std::fs::read_dir(tempdir.path())
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert!(list.is_empty());
    }

    assert_impl_all!(Directory: Send, Sync);
    assert_func_send!(Directory::open(path: PathBuf));
    assert_func_send!(Directory::create(path: PathBuf));
    assert_func_send!(Directory::create_all(path: PathBuf));

    assert_func_send!(Directory::open_at_file(&mut self, path: PathBuf));
    assert_func_send!(Directory::open_at_directory(&mut self, path: PathBuf));
    assert_func_send!(Directory::create_at_directory(&mut self, path: PathBuf));
    assert_func_send!(Directory::create_at_directory_all(&mut self, path: PathBuf));
    assert_func_send!(Directory::link_at(&mut self, link_path: PathBuf, target_path: PathBuf));
    assert_func_send!(Directory::symlink_at(
        &mut self,
        link_path: PathBuf,
        target_path: PathBuf
    ));
}
