use std::{
    convert::TryInto,
    io::{self, Read, Seek, SeekFrom, Write},
    ops::Deref,
    path::{Path, PathBuf},
};

use async_trait::async_trait;
use cealn_depset::DepMap;
use cealn_fs::Cachefile;
use compio_core::buffer::AllowTake;
use compio_fs::{os::unix::PermissionsExt, File};
use ring::digest::SHA256;

use cealn_data::{
    action::{ActionOutput, ConcreteAction},
    depmap::{DepmapHash, DepmapType},
    file_entry::{FileHash, FileHashRef},
};

use crate::{
    action::{hash_action, ActionCacheEntry, ActionDigest},
    depmap::DepmapCacheStore,
};

pub struct HotDiskCache {
    base_path: PathBuf,
    depset_registry: cealn_depset::Registry,

    content_sha256_path: PathBuf,
    action_sha256_path: PathBuf,
    depmap_sha256_path: PathBuf,
}

impl HotDiskCache {
    pub fn open(base_path: &Path, depset_registry: cealn_depset::Registry) -> anyhow::Result<HotDiskCache> {
        let base_path = base_path.to_owned();
        let mut content_sha256_path = base_path.clone();
        content_sha256_path.push("content");
        content_sha256_path.push("sha256");

        let mut action_sha256_path = base_path.clone();
        action_sha256_path.push("action");
        action_sha256_path.push("sha256");

        let mut depmap_sha256_path = base_path.clone();
        depmap_sha256_path.push("depmap");
        depmap_sha256_path.push("sha256");

        std::fs::create_dir_all(&content_sha256_path)?;

        Ok(HotDiskCache {
            base_path,
            depset_registry,
            content_sha256_path,
            action_sha256_path,
            depmap_sha256_path,
        })
    }

    pub fn root(&self) -> &Path {
        &self.base_path
    }

    pub async fn move_to_cache(&self, mut file: Cachefile) -> anyhow::Result<(FileHash, bool)> {
        let executable = crate::fs::normalize_mode(&mut file).await?.executable;
        let mut buffer = Vec::with_capacity(128 * 1024);
        let file_handle = file.ensure_open().await?;
        file_handle.seek(SeekFrom::Start(0)).await?;
        let mut hasher = ring::digest::Context::new(&SHA256);
        loop {
            let bytes_read = file_handle.read(AllowTake(&mut buffer)).await?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer);
            buffer.truncate(0);
        }
        let digest = hasher.finish();
        let filehash = FileHash::Sha256(digest.as_ref().try_into().unwrap());
        self.move_to_cache_prehashed(file, filehash.as_ref(), executable)
            .await?;
        Ok((filehash, executable))
    }

    pub async fn move_to_cache_prehashed<'a>(
        &self,
        mut file: Cachefile,
        digest: FileHashRef<'a>,
        executable: bool,
    ) -> anyhow::Result<()> {
        let dest_path = self.content_path(digest, executable);
        crate::fs::normalize_mode(&mut file).await?;
        crate::fs::link_into_cache(&mut file, &dest_path).await?;
        Ok(())
    }

    pub async fn move_to_cache_named(&self, path: &Path, normalize_mode: bool) -> anyhow::Result<(FileHash, bool)> {
        let mut file_handle = File::open(path).await?;

        let executable = if normalize_mode {
            crate::fs::normalize_mode_handle(&mut file_handle).await?.executable
        } else {
            file_handle.symlink_metadata().await?.permissions().mode() & 0o100 != 0
        };
        let mut buffer = Vec::with_capacity(128 * 1024);
        let mut hasher = ring::digest::Context::new(&SHA256);
        loop {
            let bytes_read = file_handle.read(AllowTake(&mut buffer)).await?;
            if bytes_read == 0 {
                break;
            }
            hasher.update(&buffer);
            buffer.truncate(0);
        }
        let digest = hasher.finish();
        let filehash = FileHash::Sha256(digest.as_ref().try_into().unwrap());
        let dest_path = self.content_path(filehash.as_ref(), executable);
        crate::fs::link_into_cache_handle(&mut file_handle, &dest_path).await?;
        Ok((filehash, executable))
    }

    pub async fn write_action(&self, action: &ConcreteAction, output: &ActionOutput) -> anyhow::Result<()> {
        let entry = ActionCacheEntry {
            action: action.clone(),
            output: output.clone(),
        };
        let digest = hash_action(action);
        let dest_path = self.action_path(&digest);

        let mut cachefile = cealn_fs::tempfile(&self.base_path, "action-cache-entry", false).await?;
        {
            let cachefile = cachefile.ensure_open().await?;
            let bytes = serde_json::to_vec(&entry)?;
            cachefile.write_all(bytes).await?;
        }
        crate::fs::link_into_cache(&mut cachefile, &dest_path).await?;
        Ok(())
    }

    pub async fn lookup_action(&self, action: &ConcreteAction) -> anyhow::Result<Option<ActionOutput>> {
        let digest = hash_action(action);
        let dest_path = self.action_path(&digest);

        let mut file = match File::open(&dest_path).await {
            Ok(file) => file,
            Err(ref err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };

        let mut buffer = Vec::new();
        file.read_to_end(AllowTake(&mut buffer)).await?;
        let cache_entry: ActionCacheEntry = serde_json::from_slice(&buffer)?;

        Ok(Some(cache_entry.output))
    }

    pub async fn write_depmap<I: DepmapType>(&self, depmap: &DepMap<I::Key, I::Value>) -> anyhow::Result<()> {
        DepmapCacheStore::<I>::write_depmap(self, depmap).await
    }

    pub async fn lookup_depmap<I: DepmapType>(
        &self,
        hash: &DepmapHash,
    ) -> anyhow::Result<Option<DepMap<I::Key, I::Value>>> {
        DepmapCacheStore::<I>::read_depmap(self, hash).await
    }

    pub async fn lookup_file<'a>(
        &self,
        digest: FileHashRef<'a>,
        executable: bool,
    ) -> anyhow::Result<Option<FileGuard>> {
        let content_path = self.content_path(digest, executable);
        match compio_fs::symlink_metadata(&content_path).await {
            Ok(_) => {}
            Err(ref err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        }

        // FIXME: actually guard file from cleanup
        Ok(Some(FileGuard { path: content_path }))
    }

    fn content_path(&self, digest: FileHashRef, executable: bool) -> PathBuf {
        let mut dest_file = self.content_sha256_path.clone();
        if executable {
            dest_file.push("exec");
        }
        let digest_hex = match digest {
            FileHashRef::Sha256(digest) => hex::encode(&digest),
        };
        // To keep directories from having too many entries, split up files by first byte of hash
        dest_file.push(&digest_hex[..2]);
        dest_file.push(&digest_hex);
        dest_file
    }

    fn action_path(&self, digest: &ActionDigest) -> PathBuf {
        match digest {
            ActionDigest::Sha256(digest) => {
                let mut dest_file = self.action_sha256_path.clone();
                dest_file.push(hex::encode(&digest));
                dest_file
            }
        }
    }

    fn filetree_path(&self, digest: &DepmapHash) -> PathBuf {
        match digest {
            DepmapHash::Sha256(digest) => {
                let mut dest_file = self.depmap_sha256_path.clone();
                dest_file.push(hex::encode(&digest));
                dest_file
            }
        }
    }
}

#[async_trait]
impl<I: DepmapType> DepmapCacheStore<I> for HotDiskCache {
    async fn write_depmap(&self, depmap: &DepMap<I::Key, I::Value>) -> anyhow::Result<()> {
        let dest_path = self.filetree_path(depmap.hash());

        match compio_fs::symlink_metadata(&dest_path).await {
            Ok(_) => return Ok(()),
            Err(ref err) if err.kind() == io::ErrorKind::NotFound => {
                // Need to build entry
            }
            Err(err) => return Err(err.into()),
        }

        let mut cachefile = cealn_fs::tempfile(&self.base_path, "depmap-cache-entry", false).await?;
        {
            let cachefile = cachefile.ensure_open().await?;
            DepmapCacheStore::<I>::serialize_depmap(self, cachefile, depmap).await?;
        }
        crate::fs::link_into_cache(&mut cachefile, &dest_path).await?;
        Ok(())
    }

    async fn read_depmap(&self, hash: &DepmapHash) -> anyhow::Result<Option<DepMap<I::Key, I::Value>>> {
        if let Some(registered) = self.depset_registry.get_filetree_generic::<I>(hash) {
            return Ok(Some(registered));
        }

        let src_path = self.filetree_path(hash);

        let mut file = match File::open(&src_path).await {
            Ok(file) => file,
            Err(ref err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };

        let resolved = DepmapCacheStore::<I>::deserialize_depmap(self, &mut file, hash).await?;
        if let Some(resolved) = &resolved {
            self.depset_registry.register_filetree_generic::<I>(resolved.clone());
        }
        Ok(resolved)
    }
}

pub struct FileGuard {
    path: PathBuf,
}

impl FileGuard {
    #[inline]
    pub fn path(&self) -> &Path {
        &*self.path
    }
}

impl Deref for FileGuard {
    type Target = Path;

    #[inline]
    fn deref(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use std::{convert::TryInto, io::Write};

    use super::*;

    #[test]
    fn move_to_cache() {
        let test_dir = tempfile::tempdir().unwrap();
        let cache_dir = test_dir.path().join("cache");
        let tmpfile_dir = test_dir.path().join("tmp");
        std::fs::create_dir_all(&tmpfile_dir).unwrap();
        let cache = HotDiskCache::open(&cache_dir).unwrap();

        let mut tempfile = cealn_fs::tempfile(&tmpfile_dir, "test_file").unwrap();
        let contents = b"yo";
        tempfile.open_file_mut().unwrap().write_all(contents).unwrap();
        cache.move_to_cache(tempfile).unwrap();

        let contents_hash = ring::digest::digest(&SHA256, contents);
        let contents_hash_hex = hex::encode(contents_hash.as_ref());
        let expected_path = cache_dir
            .join("content")
            .join("sha256")
            .join(&contents_hash_hex[..2])
            .join(&contents_hash_hex);
        let actual_contents = std::fs::read(&expected_path).unwrap();
        assert_eq!(&actual_contents, contents);
    }

    #[test]
    fn move_to_cache_prehashed() {
        let test_dir = tempfile::tempdir().unwrap();
        let cache_dir = test_dir.path().join("cache");
        let tmpfile_dir = test_dir.path().join("tmp");
        std::fs::create_dir_all(&tmpfile_dir).unwrap();
        let cache = HotDiskCache::open(&cache_dir).unwrap();

        let mut tempfile = cealn_fs::tempfile(&tmpfile_dir, "test_file").unwrap();
        let contents = b"yo";
        tempfile.open_file_mut().unwrap().write_all(contents).unwrap();
        let contents_hash = ring::digest::digest(&SHA256, contents);

        cache
            .move_to_cache_prehashed(
                tempfile,
                &FileHash::Sha256(contents_hash.as_ref().try_into().unwrap()),
                false,
            )
            .unwrap();

        let contents_hash_hex = hex::encode(contents_hash.as_ref());
        let expected_path = cache_dir
            .join("content")
            .join("sha256")
            .join(&contents_hash_hex[..2])
            .join(&contents_hash_hex);
        let actual_contents = std::fs::read(&expected_path).unwrap();
        assert_eq!(&actual_contents, contents);
    }
}
