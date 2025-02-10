use std::{
    borrow::Cow,
    fmt::{self, Debug},
    hash::Hash,
    mem,
};

use regex::RegexSet;
use serde::{
    de::{value::MapAccessDeserializer, DeserializeOwned, Error as DeError},
    ser::SerializeMap,
    Deserialize, Serialize,
};

use crate::{
    action::ARGUMENT_SOURCE_TEMPLATED_SENTINEL,
    file_entry::{FileEntry, FileEntryRef, FileHashRef},
    label::{LabelPath, LabelPathBuf, NormalizedDescending, LABEL_SENTINEL},
    Label, LabelBuf,
};

#[derive(Clone, PartialEq, Eq, Hash, Copy)]
pub enum DepmapHash {
    Sha256([u8; 32]),
}

pub trait DepmapType: Clone + Copy + Debug + 'static {
    type Key: ?Sized + Clone + PartialEq + Eq + Hash + DepmapKey + Serialize + DeserializeOwned + Debug + 'static;
    type Value: ?Sized + Clone + PartialEq + Eq + Hash + DepmapStorable + Serialize + DeserializeOwned + Debug + 'static;

    type DepmapReference: Clone + PartialEq + Eq + Hash + Serialize + DeserializeOwned + Debug;

    fn reference_deserialize_visit_map<'de, A>(map: A) -> Result<Self::DepmapReference, A::Error>
    where
        A: serde::de::MapAccess<'de>;
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ConcreteDepmapReference {
    pub hash: DepmapHash,
    pub subpath: Option<NormalizedDescending<LabelPathBuf>>,
}

/// A map from [`String`] paths to [`FileEntry`](crate::file_entry::FileEntry)s
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct ConcreteFiletreeType;

/// A map from [`String`] paths to [`Label`](crate::Label)s
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct LabelFiletreeType;

pub trait DepmapStorable: Send + Sync + 'static {
    type Ref<'a>: Copy + Hash + PartialEq + Eq + Debug + Send + Sync;

    fn write_bytes<'a>(r: Self::Ref<'a>, buffer: &mut Vec<u8>);
    fn from_bytes<'a>(bytes: &'a [u8]) -> Option<(Self::Ref<'a>, &'a [u8])>;
}

pub trait DepmapKey: DepmapStorable {
    type Cow<'a>;

    fn join<'a, 'b>(r: Self::Ref<'a>, v: Self::Ref<'b>) -> Self;
    fn strip_prefix<'a, 'b>(a: Self::Ref<'a>, b: Self::Ref<'b>) -> Option<Self::Ref<'a>>;
    fn is_match<'a>(patterns: &RegexSet, x: Self::Ref<'a>) -> bool;

    fn cow_owned<'a>(o: Self) -> Self::Cow<'a>;
    fn cow_borrowed<'a>(r: Self::Ref<'a>) -> Self::Cow<'a>;
    fn deref_cow<'a, 'b>(c: &'a Self::Cow<'b>) -> Self::Ref<'a>
    where
        'b: 'a;
    fn eq<'a, 'b>(a: Self::Ref<'a>, b: Self::Ref<'b>) -> bool;
}

impl DepmapType for ConcreteFiletreeType {
    type Key = NormalizedDescending<LabelPathBuf>;
    type Value = FileEntry;

    type DepmapReference = ConcreteDepmapReference;

    fn reference_deserialize_visit_map<'de, A>(map: A) -> Result<Self::DepmapReference, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        ConcreteDepmapReference::deserialize(MapAccessDeserializer::new(map))
    }
}

impl DepmapType for LabelFiletreeType {
    type Key = NormalizedDescending<LabelPathBuf>;
    type Value = LabelBuf;

    type DepmapReference = LabelBuf;

    fn reference_deserialize_visit_map<'de, A>(mut map: A) -> Result<Self::DepmapReference, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        match map.next_key::<Cow<str>>()?.as_deref() {
            Some(LABEL_SENTINEL) => {
                let value: String = map.next_value()?;
                let label = LabelBuf::try_from(value).map_err(A::Error::custom)?;
                Ok(label)
            }
            _ => Err(A::Error::custom("expected discriminator in map")),
        }
    }
}

impl ConcreteDepmapReference {
    pub fn join(&self, path: NormalizedDescending<&LabelPath>) -> ConcreteDepmapReference {
        // FIXME: handle join properly
        let new_subpath = match &self.subpath {
            Some(leading_subpath) => leading_subpath.join(path),
            None => path.to_owned(),
        };
        ConcreteDepmapReference {
            hash: self.hash.clone(),
            subpath: Some(new_subpath),
        }
    }
}

impl DepmapStorable for String {
    type Ref<'a> = &'a str;

    #[inline]
    fn write_bytes<'a>(r: Self::Ref<'a>, buffer: &mut Vec<u8>) {
        // TODO: variable length encoding
        buffer.extend_from_slice(&(r.len() as u64).to_le_bytes());
        buffer.extend_from_slice(r.as_bytes());
    }
    #[inline]
    fn from_bytes<'a>(bytes: &'a [u8]) -> Option<(&'a str, &'a [u8])> {
        const LENGTH_SIZE: usize = mem::size_of::<u64>();
        if bytes.len() < LENGTH_SIZE {
            return None;
        }
        let (length_bytes, tail) = bytes.split_array_ref::<LENGTH_SIZE>();
        let length = usize::try_from(u64::from_le_bytes(length_bytes.clone())).ok()?;
        if tail.len() < length {
            return None;
        }
        let (content, tail) = tail.split_at(length);
        let content = std::str::from_utf8(content).ok()?;
        Some((content, tail))
    }
}

const FILE_ENTRY_REGULAR_TYPECODE: u8 = 1;
const FILE_ENTRY_SYMLINK_TYPECODE: u8 = 2;
const FILE_ENTRY_DIRECTORY_TYPECODE: u8 = 3;

impl DepmapStorable for FileEntry {
    type Ref<'a> = FileEntryRef<'a>;

    #[inline]
    fn write_bytes<'a>(r: Self::Ref<'a>, buffer: &mut Vec<u8>) {
        match r {
            FileEntryRef::Regular {
                content_hash,
                executable,
            } => {
                buffer.push(FILE_ENTRY_REGULAR_TYPECODE);
                buffer.push(if executable { 1 } else { 0 });
                match content_hash {
                    crate::file_entry::FileHashRef::Sha256(digest) => {
                        buffer.extend_from_slice(digest);
                    }
                }
            }
            FileEntryRef::Symlink(target) => {
                buffer.push(FILE_ENTRY_SYMLINK_TYPECODE);
                buffer.extend_from_slice(&(target.len() as u64).to_le_bytes());
                buffer.extend_from_slice(target.as_bytes());
            }
            FileEntryRef::Directory => buffer.push(FILE_ENTRY_DIRECTORY_TYPECODE),
        }
    }
    #[inline]
    fn from_bytes<'a>(bytes: &'a [u8]) -> Option<(FileEntryRef<'a>, &'a [u8])> {
        const LENGTH_BYTE_COUNT: usize = mem::size_of::<u64>();

        let (&typecode, tail) = bytes.split_first()?;
        match typecode {
            FILE_ENTRY_REGULAR_TYPECODE => {
                if tail.len() < 1 + 32 {
                    return None;
                }
                let (&executable, tail) = tail.split_first()?;
                let (digest, tail) = tail.split_array_ref::<32>();
                Some((
                    FileEntryRef::Regular {
                        content_hash: FileHashRef::Sha256(digest),
                        executable: executable != 0,
                    },
                    tail,
                ))
            }
            FILE_ENTRY_SYMLINK_TYPECODE => {
                if tail.len() < LENGTH_BYTE_COUNT {
                    return None;
                }
                let (len_bytes, tail) = tail.split_array_ref::<LENGTH_BYTE_COUNT>();
                let len = u64::from_le_bytes(*len_bytes) as usize;
                if tail.len() < len {
                    return None;
                }
                let (content, tail) = tail.split_at(len as usize);
                let content = std::str::from_utf8(content).ok()?;
                Some((FileEntryRef::Symlink(content), tail))
            }
            FILE_ENTRY_DIRECTORY_TYPECODE => Some((FileEntryRef::Directory, tail)),
            _ => None,
        }
    }
}

impl DepmapStorable for LabelBuf {
    type Ref<'a> = &'a Label;

    #[inline]
    fn write_bytes<'a>(r: Self::Ref<'a>, buffer: &mut Vec<u8>) {
        // TODO: variable length encoding
        buffer.extend_from_slice(&(r.len() as u64).to_le_bytes());
        buffer.extend_from_slice(r.as_str().as_bytes());
    }
    #[inline]
    fn from_bytes<'a>(bytes: &'a [u8]) -> Option<(&'a Label, &'a [u8])> {
        const LENGTH_SIZE: usize = mem::size_of::<u64>();
        if bytes.len() < LENGTH_SIZE {
            return None;
        }
        let (length_bytes, tail) = bytes.split_array_ref::<LENGTH_SIZE>();
        let length = usize::try_from(u64::from_le_bytes(length_bytes.clone())).ok()?;
        if tail.len() < length {
            return None;
        }
        let (content, tail) = tail.split_at(length);
        let content = std::str::from_utf8(content).ok()?;
        let content = Label::new(content).ok()?;
        Some((content, tail))
    }
}

impl DepmapStorable for LabelPathBuf {
    type Ref<'a> = &'a LabelPath;

    #[inline]
    fn write_bytes<'a>(r: Self::Ref<'a>, buffer: &mut Vec<u8>) {
        // TODO: variable length encoding
        buffer.extend_from_slice(&(r.len() as u64).to_le_bytes());
        buffer.extend_from_slice(r.as_str().as_bytes());
    }
    #[inline]
    fn from_bytes<'a>(bytes: &'a [u8]) -> Option<(&'a LabelPath, &'a [u8])> {
        const LENGTH_SIZE: usize = mem::size_of::<u64>();
        if bytes.len() < LENGTH_SIZE {
            return None;
        }
        let (length_bytes, tail) = bytes.split_array_ref::<LENGTH_SIZE>();
        let length = usize::try_from(u64::from_le_bytes(length_bytes.clone())).ok()?;
        if tail.len() < length {
            return None;
        }
        let (content, tail) = tail.split_at(length);
        let content = std::str::from_utf8(content).ok()?;
        let content = LabelPath::new(content).ok()?;
        Some((content, tail))
    }
}

impl DepmapStorable for NormalizedDescending<LabelPathBuf> {
    type Ref<'a> = NormalizedDescending<&'a LabelPath>;

    #[inline]
    fn write_bytes<'a>(r: Self::Ref<'a>, buffer: &mut Vec<u8>) {
        // TODO: variable length encoding
        buffer.extend_from_slice(&(r.len() as u64).to_le_bytes());
        buffer.extend_from_slice(r.as_str().as_bytes());
    }
    #[inline]
    fn from_bytes<'a>(bytes: &'a [u8]) -> Option<(NormalizedDescending<&'a LabelPath>, &'a [u8])> {
        const LENGTH_SIZE: usize = mem::size_of::<u64>();
        if bytes.len() < LENGTH_SIZE {
            return None;
        }
        let (length_bytes, tail) = bytes.split_array_ref::<LENGTH_SIZE>();
        let length = usize::try_from(u64::from_le_bytes(length_bytes.clone())).ok()?;
        if tail.len() < length {
            return None;
        }
        let (content, tail) = tail.split_at(length);
        let content = std::str::from_utf8(content).ok()?;
        let content = LabelPath::new(content).ok()?;
        let content = content.require_normalized_descending()?;
        Some((content, tail))
    }
}

impl DepmapKey for NormalizedDescending<LabelPathBuf> {
    type Cow<'a> = NormalizedDescending<Cow<'a, LabelPath>>;

    #[inline]
    fn join<'a, 'b>(a: Self::Ref<'a>, b: Self::Ref<'b>) -> Self {
        a.join(b)
    }

    fn strip_prefix<'a, 'b>(a: Self::Ref<'a>, b: Self::Ref<'b>) -> Option<Self::Ref<'a>> {
        a.strip_prefix(&b)
    }

    fn is_match<'a>(patterns: &RegexSet, x: Self::Ref<'a>) -> bool {
        patterns.is_match(x.as_str())
    }

    #[inline]
    fn cow_owned<'a>(o: Self) -> Self::Cow<'a> {
        o.into_cow()
    }

    #[inline]
    fn cow_borrowed<'a>(r: Self::Ref<'a>) -> Self::Cow<'a> {
        r.as_cow()
    }

    #[inline]
    fn deref_cow<'a, 'b>(c: &'a Self::Cow<'b>) -> Self::Ref<'a>
    where
        'b: 'a,
    {
        c.as_ref()
    }

    #[inline]
    fn eq<'a, 'b>(a: Self::Ref<'a>, b: Self::Ref<'b>) -> bool {
        a == b
    }
}

impl DepmapStorable for () {
    type Ref<'a> = ();

    #[inline]
    fn write_bytes<'a>(_r: Self::Ref<'a>, _buffer: &mut Vec<u8>) {}
    #[inline]
    fn from_bytes<'a>(bytes: &'a [u8]) -> Option<((), &'a [u8])> {
        Some(((), bytes))
    }
}

const SHA256_PREFIX: &str = "sha256:";

impl Serialize for DepmapHash {
    #[inline]
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            match self {
                DepmapHash::Sha256(sha256) => {
                    let mut buffer = [0u8; SHA256_PREFIX.len() + 32 * 2];
                    buffer[..SHA256_PREFIX.len()].copy_from_slice(SHA256_PREFIX.as_bytes());
                    hex::encode_to_slice(&sha256, &mut buffer[SHA256_PREFIX.len()..]).unwrap();
                    serializer.serialize_str(std::str::from_utf8(&buffer).unwrap())
                }
            }
        } else {
            match self {
                DepmapHash::Sha256(sha256) => serializer.serialize_newtype_variant("depmap_hash", 0, "sha256", &sha256),
            }
        }
    }
}

impl<'de> Deserialize<'de> for DepmapHash {
    #[inline]
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            deserializer.deserialize_str(ReadableDigestVisior {})
        } else {
            todo!()
        }
    }
}

struct ReadableDigestVisior {}

impl serde::de::Visitor<'_> for ReadableDigestVisior {
    type Value = DepmapHash;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "digest starting with 'sha256:'")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if let Some(hex_digest) = v.strip_prefix(SHA256_PREFIX) {
            if hex_digest.len() != 64 {
                return Err(E::custom("invalid sha256 digest length"));
            }
            let mut digest = [0u8; 32];
            hex::decode_to_slice(hex_digest, &mut digest).unwrap();
            Ok(DepmapHash::Sha256(digest))
        } else {
            Err(E::custom("invalid digest prefix"))
        }
    }
}

impl From<DepmapHash> for ConcreteDepmapReference {
    #[inline]
    fn from(value: DepmapHash) -> Self {
        ConcreteDepmapReference {
            hash: value,
            subpath: None,
        }
    }
}

impl fmt::Debug for DepmapHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Sha256(digest) => write!(f, "sha256:{}", hex::encode(digest)),
        }
    }
}
