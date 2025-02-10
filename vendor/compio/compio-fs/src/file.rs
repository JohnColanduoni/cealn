use std::{
    io::{self, Result, SeekFrom},
    mem::ManuallyDrop,
    path::Path,
    pin::Pin,
    task::Context,
};

use compio_core::{
    buffer::{AllowTake, OutputBuffer, RawInputBuffer, RawOutputBuffer, SliceableRawOutputBuffer},
    io::{AsyncRead, AsyncWrite, AsyncWriteExt},
};
use futures::{future::BoxFuture, prelude::*};

use crate::{platform, Metadata, OpenOptions};

pub struct File {
    pub(crate) imp: platform::File,
}

impl File {
    #[inline]
    pub async fn open(path: impl AsRef<Path>) -> Result<File> {
        OpenOptions::new().read(true).open(path).await
    }

    #[inline]
    pub async fn create(path: impl AsRef<Path>) -> Result<File> {
        OpenOptions::new()
            .write(true)
            .truncate(true)
            .create(true)
            .open(path)
            .await
    }

    #[tracing::instrument(level = "trace", err, skip(self, buffer))]
    #[inline]
    pub async fn read<'a>(&'a mut self, buffer: impl RawInputBuffer + 'a) -> Result<usize> {
        self.imp.read(buffer).await
    }

    #[tracing::instrument(level = "trace", err, skip(self, buffer))]
    #[inline]
    pub async fn write<'a>(&'a mut self, buffer: impl RawOutputBuffer + 'a) -> Result<usize> {
        self.imp.write(buffer).await
    }

    #[inline]
    pub async fn seek<'a>(&'a mut self, pos: SeekFrom) -> Result<u64> {
        self.imp.seek(pos).await
    }

    #[inline]
    pub async fn symlink_metadata<'a>(&'a mut self) -> Result<Metadata> {
        let imp = self.imp.symlink_metadata().await?;
        Ok(Metadata { imp })
    }

    // FIXME: generalize buffer type
    #[tracing::instrument(level = "trace", err, skip(self, buffer))]
    pub async fn read_to_end<'a>(&'a mut self, buffer: AllowTake<&'a mut Vec<u8>>) -> Result<usize> {
        let mut total_bytes_read = 0;
        loop {
            if buffer.0.len() == buffer.0.capacity() {
                buffer.0.reserve(128 * 1024);
            }
            let bytes_read = self.read(AllowTake(&mut *buffer.0)).await?;
            if bytes_read == 0 {
                break;
            }
            total_bytes_read += bytes_read;
        }
        Ok(total_bytes_read)
    }

    #[tracing::instrument(level = "trace", err, skip(self, buffer))]
    #[inline]
    pub async fn write_all<'a, O>(&'a mut self, buffer: O) -> Result<()>
    where
        O: SliceableRawOutputBuffer + 'a,
    {
        AsyncWriteExt::write_all(self, buffer).await
    }

    // This is necessary in some situations due to a bug in rust's higher-kinded lifetime resolution. Remove it once
    // that is fixed
    pub fn write_all_mono<'a>(&'a mut self, buffer: &'a mut Vec<u8>) -> BoxFuture<'a, Result<()>> {
        unsafe {
            let boxed: Pin<Box<dyn Future<Output = Result<()>>>> = Box::pin(self.write_all(AllowTake(buffer)));
            std::mem::transmute(boxed)
        }
    }
}

pub async fn remove_file(path: impl AsRef<Path>) -> Result<()> {
    platform::remove_file(path.as_ref()).await
}

pub async fn rename(src: impl AsRef<Path>, dest: impl AsRef<Path>) -> Result<()> {
    platform::rename(src.as_ref(), dest.as_ref()).await
}

pub async fn symlink_metadata(path: impl AsRef<Path>) -> Result<Metadata> {
    let imp = platform::symlink_metadata(path.as_ref()).await?;
    Ok(Metadata { imp })
}

pub async fn read(path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let mut file = File::open(path.as_ref()).await?;
    let mut buffer = Vec::new();
    file.read_to_end(AllowTake(&mut buffer)).await?;
    Ok(buffer)
}

impl AsyncRead for File {
    type Read<'a, I: RawInputBuffer + 'a> = impl Future<Output = Result<usize>> + Send + 'a;

    fn read<'a, I>(&'a mut self, buffer: I) -> Self::Read<'a, I>
    where
        I: RawInputBuffer + 'a,
    {
        File::read(self, buffer)
    }
}

impl AsyncWrite for File {
    type Write<'a, O: RawOutputBuffer + 'a> = impl Future<Output = Result<usize>> + Send + 'a;

    fn write<'a, O>(&'a mut self, buffer: O) -> Self::Write<'a, O>
    where
        O: RawOutputBuffer + 'a,
    {
        File::write(self, buffer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::{
        fs::File as StdFile,
        io::{Read, Write},
        mem,
        path::PathBuf,
    };

    use compio_core::buffer::AllowTake;
    use compio_executor::LocalPool;
    use compio_internal_util::assert_func_send;
    use static_assertions::assert_impl_all;
    use tempfile::TempDir;

    #[test]
    fn open_file() {
        let tempdir = TempDir::new().unwrap();
        let file_path = tempdir.path().join("testfile.txt");
        mem::drop(StdFile::create(&file_path).unwrap());

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let _file = File::open(&file_path).await.unwrap();
        });
    }

    #[test]
    fn read_file() {
        const TEST_STRING: &str = "Hello World!";

        let tempdir = TempDir::new().unwrap();
        let file_path = tempdir.path().join("testfile.txt");
        {
            let mut std_file = StdFile::create(&file_path).unwrap();
            std_file.write_all(TEST_STRING.as_bytes()).unwrap();
        }

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let mut file = File::open(&file_path).await.unwrap();
            let mut buffer = Vec::with_capacity(4096);
            file.read(AllowTake(&mut buffer)).await.unwrap();
            assert_eq!(&*buffer, TEST_STRING.as_bytes());
        });
    }

    #[test]
    fn read_to_end_file() {
        const TEST_STRING: &str = "Hello World!";

        let tempdir = TempDir::new().unwrap();
        let file_path = tempdir.path().join("testfile.txt");
        {
            let mut std_file = StdFile::create(&file_path).unwrap();
            std_file.write_all(TEST_STRING.as_bytes()).unwrap();
        }

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let mut file = File::open(&file_path).await.unwrap();
            let mut buffer = Vec::with_capacity(0);
            file.read_to_end(AllowTake(&mut buffer)).await.unwrap();
            assert_eq!(&*buffer, TEST_STRING.as_bytes());
        });
    }

    #[test]
    fn write_file() {
        const TEST_STRING: &str = "Hello World!";

        let tempdir = TempDir::new().unwrap();
        let file_path = tempdir.path().join("testfile.txt");
        {
            let _std_file = StdFile::create(&file_path).unwrap();
        }

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let mut file = OpenOptions::new().write(true).open(&file_path).await.unwrap();
            let mut buffer = TEST_STRING.as_bytes().to_owned();
            file.write_all(AllowTake(&mut buffer)).await.unwrap();
        });

        {
            let mut std_file = StdFile::open(&file_path).unwrap();
            let mut buffer = Vec::new();
            std_file.read_to_end(&mut buffer).unwrap();
            assert_eq!(&*buffer, TEST_STRING.as_bytes());
        }
    }

    #[test]
    fn metadata() {
        let tempdir = TempDir::new().unwrap();
        let file_path = tempdir.path().join("testfile.txt");
        {
            let _std_file = StdFile::create(&file_path).unwrap();
        }

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            let mut file = OpenOptions::new().read(true).open(&file_path).await.unwrap();
            let _metadata = file.symlink_metadata().await.unwrap();
        });
    }

    #[test]
    fn rename_directory() {
        let tempdir = TempDir::new().unwrap();
        let subdir_path = tempdir.path().join("testdir");
        let subdir_dest_path = tempdir.path().join("testdir2");
        std::fs::create_dir(&subdir_path).unwrap();

        let mut executor = LocalPool::new().unwrap();
        executor.run_until(async {
            rename(&subdir_path, &subdir_dest_path).await.unwrap();
        });

        assert!(subdir_dest_path.is_dir());
        assert!(!subdir_path.exists());
    }

    assert_impl_all!(File: Send, Sync);
    assert_func_send!(File::open(path: PathBuf));
    assert_func_send!(File::create(path: PathBuf));
    assert_func_send!(File::read(&mut self, buffer: AllowTake<&'static mut Vec<u8>>));
    assert_func_send!(File::write(&mut self, buffer: AllowTake<&'static mut Vec<u8>>));
    assert_func_send!(File::symlink_metadata(&mut self,));
    assert_func_send!(File::read_to_end(&mut self, buffer: AllowTake<&'static mut Vec<u8>>));
    assert_func_send!(File::write_all(&mut self, buffer: AllowTake<&'static mut Vec<u8>>));
    assert_func_send!(rename(path1: PathBuf, path2: PathBuf));
}
