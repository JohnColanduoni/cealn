use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum FileEntry {
    Regular { content_hash: FileHash, executable: bool },
    Symlink(String),
    Directory,
}

#[derive(Clone, Copy, Hash, Debug)]
pub enum FileEntryRef<'a> {
    Regular {
        content_hash: FileHashRef<'a>,
        executable: bool,
    },
    Symlink(&'a str),
    Directory,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum FileType {
    Regular,
    Symlink,
    Directory,
}

#[derive(Clone, PartialEq, Eq, Hash)]
pub enum FileHash {
    Sha256([u8; 32]),
}

#[derive(Clone, Copy, Hash)]
pub enum FileHashRef<'a> {
    Sha256(&'a [u8; 32]),
}

#[cfg(not(target_arch = "wasm32"))]
impl From<ring::digest::Digest> for FileHash {
    fn from(value: ring::digest::Digest) -> Self {
        if value.algorithm() == &ring::digest::SHA256 {
            FileHash::Sha256(value.as_ref().try_into().unwrap())
        } else {
            panic!("invalid file hash algorithm");
        }
    }
}

const SHA256_PREFIX: &str = "sha256:";

impl FileEntry {
    pub fn as_ref(&self) -> FileEntryRef {
        match self {
            FileEntry::Regular {
                content_hash,
                executable,
            } => FileEntryRef::Regular {
                content_hash: content_hash.as_ref(),
                executable: *executable,
            },
            FileEntry::Symlink(target) => FileEntryRef::Symlink(target),
            FileEntry::Directory => FileEntryRef::Directory,
        }
    }
}

impl<'a> FileEntryRef<'a> {
    pub fn to_owned(&self) -> FileEntry {
        match *self {
            FileEntryRef::Regular {
                content_hash,
                executable,
            } => FileEntry::Regular {
                content_hash: content_hash.to_owned(),
                executable,
            },
            FileEntryRef::Symlink(target) => FileEntry::Symlink(target.to_owned()),
            FileEntryRef::Directory => FileEntry::Directory,
        }
    }
}

impl FileHash {
    pub fn as_ref(&self) -> FileHashRef {
        match self {
            FileHash::Sha256(r) => FileHashRef::Sha256(r),
        }
    }
}

impl<'a> FileHashRef<'a> {
    pub fn to_owned(&self) -> FileHash {
        match *self {
            FileHashRef::Sha256(digest) => FileHash::Sha256(digest.clone()),
        }
    }
}

impl<'a, 'b> PartialEq<FileEntryRef<'b>> for FileEntryRef<'a> {
    fn eq(&self, other: &FileEntryRef<'b>) -> bool {
        match (self, other) {
            (
                Self::Regular {
                    content_hash: l_content_hash,
                    executable: l_executable,
                },
                FileEntryRef::Regular {
                    content_hash: r_content_hash,
                    executable: r_executable,
                },
            ) => l_content_hash == r_content_hash && *l_executable == *r_executable,
            (Self::Symlink(l0), FileEntryRef::Symlink(r0)) => l0 == r0,
            _ => core::mem::discriminant(self) == core::mem::discriminant(other),
        }
    }
}

impl<'a> Eq for FileEntryRef<'a> {}

impl<'a, 'b> PartialEq<FileHashRef<'b>> for FileHashRef<'a> {
    fn eq(&self, other: &FileHashRef<'b>) -> bool {
        match (self, other) {
            (FileHashRef::Sha256(l0), FileHashRef::Sha256(r0)) => **l0 == **r0,
        }
    }
}

impl<'a> Eq for FileHashRef<'a> {}

impl Serialize for FileHash {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            FileHash::Sha256(bytes) => {
                let mut buffer = [0u8; SHA256_PREFIX.len() + 32 * 2];
                buffer[..SHA256_PREFIX.len()].copy_from_slice(SHA256_PREFIX.as_bytes());
                hex::encode_to_slice(bytes, &mut buffer[SHA256_PREFIX.len()..]).unwrap();
                serializer.serialize_str(unsafe { std::str::from_utf8_unchecked(&buffer) })
            }
        }
    }
}

impl<'de> Deserialize<'de> for FileHash {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(FileHashVisitor {})
    }
}

struct FileHashVisitor {}

impl<'de> serde::de::Visitor<'de> for FileHashVisitor {
    type Value = FileHash;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "file hash")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        let hex_str = v
            .strip_prefix(SHA256_PREFIX)
            .ok_or_else(|| E::custom("invalid file hash prefix"))?;
        let mut buffer = [0u8; 32];
        hex::decode_to_slice(hex_str.as_bytes(), &mut buffer).map_err(|_| E::custom("invalid hex in file hash"))?;
        Ok(FileHash::Sha256(buffer))
    }
}

impl fmt::Debug for FileHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sha256(data) => write!(f, "sha256:{}", hex::encode(&data)),
        }
    }
}

impl fmt::Debug for FileHashRef<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sha256(data) => write!(f, "sha256:{}", hex::encode(data)),
        }
    }
}
