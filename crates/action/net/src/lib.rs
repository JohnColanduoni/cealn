use std::{convert::TryInto, io::Write};

use anyhow::{anyhow, bail};
use cealn_depset::ConcreteFiletree;
use cealn_protocol::event::BuildEventData;
use futures::prelude::*;
use ring::digest::SHA256;
use tracing::{debug_span, error, Instrument};

use cealn_action_context::Context;
use cealn_data::{
    action::{ActionOutput, Download, DownloadFileDigest},
    file_entry::{FileEntry, FileEntryRef, FileHash},
};

#[tracing::instrument("run_download", level = "debug", err, skip(context))]
pub async fn download<C: Context>(context: &C, action: &Download) -> anyhow::Result<ActionOutput> {
    let mut events = context.events().fork();

    let mut downloaded = None;
    for url in action.urls.iter() {
        let span = debug_span!("download_attempt", %url);
        match async {
            let mut request_builder = context.http_client().get(url);
            if action.user_agent != "cealn" {
                request_builder = request_builder.header("User-Agent", &action.user_agent);
            }
            let mut response = request_builder.send().await?.error_for_status()?;

            let mut total_bytes_read = 0;
            let content_length = response.content_length();

            let mut tempfile = context.tempfile("http_download", action.executable).await?;
            let mut hasher = ring::digest::Context::new(&SHA256);
            {
                let writer = tempfile.ensure_open().await?;
                while let Some(chunk) = response.chunk().await? {
                    hasher.update(&chunk);
                    total_bytes_read += chunk.len() as u64;
                    writer.write_all(chunk).await?;
                    if let Some(content_length) = content_length {
                        events.send(BuildEventData::Progress {
                            fraction: total_bytes_read as f64 / content_length as f64,
                        });
                    }
                }
            }

            let digest = DownloadFileDigest::Sha256(hasher.finish().as_ref().try_into().unwrap());

            if let Some(expected_digest) = &action.digest {
                if expected_digest != &digest {
                    bail!(
                        "invalid download digest, expected {:?} but got {:?}",
                        expected_digest,
                        digest
                    )
                }
            }

            Ok((tempfile, digest))
        }
        .instrument(span)
        .await
        {
            Ok(result) => {
                downloaded = Some(result);
                break;
            }
            Err(err) => {
                error!("download attempt failed: {}", err);
                events.send_stderr(format!("download attempt failed: {}", err));
                continue;
            }
        }
    }

    let (tempfile, digest) =
        downloaded.ok_or_else(|| anyhow!("none of the provided URLs could be downloaded from succesfully"))?;

    let file_hash = match digest {
        DownloadFileDigest::Sha256(bytes) => FileHash::Sha256(bytes),
    };

    context
        .move_to_cache_prehashed(tempfile, file_hash.as_ref(), action.executable)
        .await?;

    let depmap = ConcreteFiletree::builder()
        .insert(
            action.filename.as_ref(),
            FileEntryRef::Regular {
                content_hash: file_hash.as_ref(),
                executable: action.executable,
            },
        )
        .build();
    let depmap_reference = context.register_concrete_filetree_depmap(depmap).await?;

    Ok(ActionOutput {
        files: depmap_reference,

        stdout: None,
        stderr: None,
    })
}
