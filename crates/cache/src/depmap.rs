use std::mem;

use anyhow::bail;
use async_trait::async_trait;
use bytes::Buf;
use cealn_data::depmap::{DepmapHash, DepmapType};
use cealn_depset::DepMap;
use compio_core::{
    buffer::{AllowCopy, AllowTake},
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
};
use futures::{
    future::{BoxFuture, LocalBoxFuture},
    FutureExt,
};
use tracing::error;

#[async_trait]
pub trait DepmapCacheStore<I: DepmapType>: Send + Sync {
    async fn write_depmap(&self, depmap: &DepMap<I::Key, I::Value>) -> anyhow::Result<()>;
    async fn read_depmap(&self, hash: &DepmapHash) -> anyhow::Result<Option<DepMap<I::Key, I::Value>>>;

    async fn serialize_depmap<'a, W>(
        &'a self,
        write: &'a mut W,
        depmap: &'a DepMap<I::Key, I::Value>,
    ) -> anyhow::Result<()>
    where
        W: AsyncWrite + Send + 'static,
    {
        for sub_depmap in depmap.transitive_iter() {
            self.write_depmap(&sub_depmap).await?;
        }

        // FIXME: don't copy
        write.write_all(AllowCopy(depmap.serialized_bytes())).await?;
        Ok(())
    }

    fn deserialize_depmap<'a, R>(
        &'a self,
        read: &'a mut R,
        hash: &DepmapHash,
    ) -> BoxFuture<'a, anyhow::Result<Option<DepMap<I::Key, I::Value>>>>
    where
        R: AsyncRead + Send + 'static,
    {
        let hash = hash.clone();
        async move {
            let mut buffer = Vec::with_capacity(128 * 1024);
            read.read_to_end(AllowTake(&mut buffer)).await?;

            let mut transitive_depmaps = Vec::new();
            let depmap_hashes = match cealn_depset::depmap::scan_transitive_nodes::<I::Key, I::Value>(&buffer) {
                Ok(depmap_hashes) => depmap_hashes,
                Err(err) => {
                    // We treat this the same as a cache miss
                    // FIXME: report this to user
                    error!(hash=?hash, "corrupted depmap: {}", err);
                    return Ok(None);
                }
            };
            for depmap_hash in depmap_hashes {
                let Some(depmap) = self.read_depmap(&depmap_hash).await? else {
                    return Ok(None);
                };
                transitive_depmaps.push(depmap);
            }

            Ok(Some(DepMap::deserialize(buffer, transitive_depmaps, hash)))
        }
        .boxed()
    }
}
