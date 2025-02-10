use serde::{Deserialize, Serialize};

use crate::label::{LabelPathBuf, NormalizedDescending};

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct Download {
    pub filename: NormalizedDescending<LabelPathBuf>,
    pub urls: Vec<String>,
    pub executable: bool,
    pub digest: Option<DownloadFileDigest>,

    pub user_agent: String,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum DownloadFileDigest {
    Sha256([u8; 32]),
}

const SHA256_PREFIX: &str = "sha256:";

impl Serialize for DownloadFileDigest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        if serializer.is_human_readable() {
            match self {
                DownloadFileDigest::Sha256(digest) => {
                    let mut buffer = [0u8; SHA256_PREFIX.len() + 32 * 2];
                    buffer[..SHA256_PREFIX.len()].copy_from_slice(SHA256_PREFIX.as_bytes());
                    hex::encode_to_slice(digest, &mut buffer[SHA256_PREFIX.len()..]).unwrap();
                    serializer.serialize_str(std::str::from_utf8(&buffer).unwrap())
                }
            }
        } else {
            match self {
                DownloadFileDigest::Sha256(digest) => {
                    serializer.serialize_newtype_variant("download_file_digest", 0, "sha256", digest)
                }
            }
        }
    }
}

impl<'de> Deserialize<'de> for DownloadFileDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            deserializer.deserialize_str(ReadableDigestVisior)
        } else {
            todo!()
        }
    }
}

struct ReadableDigestVisior;

impl serde::de::Visitor<'_> for ReadableDigestVisior {
    type Value = DownloadFileDigest;

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
            Ok(DownloadFileDigest::Sha256(digest))
        } else {
            Err(E::custom("invalid digest prefix"))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::convert::TryInto;

    use super::*;

    #[test]
    fn serialize_digest_json() {
        let json = serde_json::to_string(&DownloadFileDigest::Sha256(
            hex::decode("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                .unwrap()
                .try_into()
                .unwrap(),
        ))
        .unwrap();
        assert_eq!(
            json,
            r#""sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855""#
        );
    }

    #[test]
    fn deserialize_digest_json() {
        let digest: DownloadFileDigest =
            serde_json::from_str(r#""sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855""#)
                .unwrap();
        assert_eq!(
            digest,
            DownloadFileDigest::Sha256(
                hex::decode("e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855")
                    .unwrap()
                    .try_into()
                    .unwrap()
            )
        )
    }
}
