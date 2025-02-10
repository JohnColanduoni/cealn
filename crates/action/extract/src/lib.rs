#![feature(read_buf)]

use std::{
    borrow::Cow,
    ffi::OsString,
    io::{self, Cursor, Read, Seek, Write},
    mem,
    pin::Pin,
    task::Poll,
};

use anyhow::{anyhow, bail, Context as AnyhowContext};
use cealn_depset::{ConcreteFiletree, DepMap};
use compio_core::{
    buffer::AllowTake,
    io::{AsyncRead as CompioAsyncRead, PollableRead},
};
use compio_fs::File;
use futures::{pin_mut, prelude::*, AsyncRead, TryStreamExt};
use ring::digest::SHA256;
use tokio::io::AsyncReadExt;

use cealn_action_context::Context;
use cealn_data::{
    action::{ActionOutput, Extract},
    depmap::ConcreteFiletreeType,
    file_entry::{FileEntry, FileEntryRef, FileHash},
    label::{LabelPath, LabelPathBuf},
};

#[tracing::instrument(level = "debug", err, skip(context))]
pub async fn extract<C: Context>(context: &C, action: &Extract<ConcreteFiletreeType>) -> anyhow::Result<ActionOutput> {
    let input_file = context.open_depmap_file(&action.archive).await?;
    let mut input_file = File::open(&*input_file).await?;

    // Identity file type
    let mut header_buffer = Vec::with_capacity(4096);
    let header_buffer_len = input_file.read(AllowTake(&mut header_buffer)).await?;
    input_file.seek(std::io::SeekFrom::Start(0)).await?;

    let input_file = input_file.pollable_read();
    pin_mut!(input_file);
    match infer::get(&header_buffer).map(|x| x.mime_type()) {
        Some("application/gzip") => gz_extract(context, action, input_file).await,
        Some("application/x-xz") => xz_extract(context, action, input_file).await,
        Some("application/zip") => zip_extract(context, action, input_file).await,
        Some("application/zstd") => zstd_extract(context, action, input_file).await,
        Some(mime_type) => bail!("unsupported archive type {:?}", mime_type),
        None => bail!("failed to detect archive type"),
    }
}

async fn gz_extract<'a, 'b, C: Context>(
    context: &C,
    action: &Extract<ConcreteFiletreeType>,
    mut stream: Pin<&'a mut PollableRead<'b, File>>,
) -> anyhow::Result<ActionOutput> {
    // FIXME: use async here
    let mut buffer = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buffer).await?;

    let mut decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(buffer));
    let mut decoded_buffer = Vec::new();
    decoder.read_to_end(&mut decoded_buffer)?;
    let decoded_cursor = std::io::Cursor::new(decoded_buffer);
    pin_mut!(decoded_cursor);
    extract_inner(context, action, decoded_cursor).await
}

async fn xz_extract<'a, 'b, C: Context>(
    context: &C,
    action: &Extract<ConcreteFiletreeType>,
    mut stream: Pin<&'a mut PollableRead<'b, File>>,
) -> anyhow::Result<ActionOutput> {
    // FIXME: use async here
    let mut buffer = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buffer).await?;

    let decoder = XzDecoderWrapper(xz2::read::XzDecoder::new(std::io::Cursor::new(buffer)));
    pin_mut!(decoder);
    extract_inner(context, action, decoder).await
}

async fn zstd_extract<'a, 'b, C: Context>(
    context: &C,
    action: &Extract<ConcreteFiletreeType>,
    mut stream: Pin<&'a mut PollableRead<'b, File>>,
) -> anyhow::Result<ActionOutput> {
    // FIXME: use async here
    let mut buffer = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut stream, &mut buffer).await?;

    let mut decoder = zstd::stream::read::Decoder::new(std::io::Cursor::new(buffer))?;
    let mut decoded_buffer = Vec::new();
    decoder.read_to_end(&mut decoded_buffer)?;
    let decoded_cursor = std::io::Cursor::new(decoded_buffer);
    pin_mut!(decoded_cursor);
    extract_inner(context, action, decoded_cursor).await
}

struct XzDecoderWrapper<R: std::io::Read>(xz2::read::XzDecoder<R>);

impl<R: tokio::io::AsyncRead + std::io::Read> tokio::io::AsyncRead for XzDecoderWrapper<R> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let buffer = buf.initialize_unfilled();
        match std::io::Read::read(unsafe { &mut self.get_unchecked_mut().0 }, buffer) {
            Ok(len) => {
                buf.advance(len);
                Poll::Ready(Ok(()))
            }
            Err(ref err) if err.kind() == io::ErrorKind::WouldBlock => Poll::Pending,
            Err(err) => Poll::Ready(Err(err)),
        }
    }
}

async fn extract_inner<'a, C: Context, R: tokio::io::AsyncRead>(
    context: &C,
    action: &Extract<ConcreteFiletreeType>,
    mut stream: Pin<&'a mut R>,
) -> anyhow::Result<ActionOutput> {
    // Identity file type
    let mut header_buffer = vec![0u8; 4096];
    let header_buffer_len = stream.read(&mut header_buffer).await?;
    header_buffer.truncate(header_buffer_len);
    let file_type = infer::get(&header_buffer);
    let mut leading_stream = LeadingStream {
        leading: Cursor::new(header_buffer),
        remaining: stream,
    };
    pin_mut!(leading_stream);

    match file_type.map(|x| x.mime_type()) {
        Some("application/x-tar") => tar_extract::<C, LeadingStream<_>>(context, action, leading_stream).await,
        Some(mime_type) => bail!("unsupported archive type {:?}", mime_type),
        None => bail!("failed to detect archive type"),
    }
}

async fn tar_extract<C: Context, R: AsyncRead>(
    context: &C,
    action: &Extract<ConcreteFiletreeType>,
    stream: Pin<&mut R>,
) -> anyhow::Result<ActionOutput> {
    let mut depmap = ConcreteFiletree::builder();
    let archive = async_tar::Archive::new(stream);

    let mut buffer = vec![0u8; 128 * 1024];

    let mut entries = archive.entries()?;
    while let Some(mut entry) = entries.try_next().await? {
        let filename =
            String::from_utf8(entry.path_bytes().into_owned()).context("invalid utf8 in archive filename")?;
        let filename = LabelPathBuf::new(filename).with_context(|| {
            format!(
                "invalid label path {:?} in archive filename",
                String::from_utf8_lossy(&entry.path_bytes())
            )
        })?;

        let filename = filename
            .normalize_require_descending()
            .context("archive filename escapes root")?;

        let filename = if let Some(strip_prefix) = &action.strip_prefix {
            let Some(remaining_path) = filename.strip_prefix(&*strip_prefix) else {
                continue;
            };
            remaining_path.to_owned()
        } else {
            filename.into_owned()
        };

        match entry.header().entry_type() {
            async_tar::EntryType::Regular => {
                let executable = (entry.header().mode()? & 0o100) != 0;
                let mut cache_file = context.tempfile("archive-file", executable).await?;
                let mut hasher = ring::digest::Context::new(&SHA256);

                {
                    let cache_file = cache_file.ensure_open().await?;

                    loop {
                        let bytes_read = entry.read(&mut buffer).await?;
                        if bytes_read == 0 {
                            break;
                        }
                        buffer.truncate(bytes_read);
                        hasher.update(&buffer);
                        cache_file.write_all_mono(&mut buffer).await?;
                        unsafe {
                            buffer.set_len(buffer.capacity());
                        }
                    }
                }

                let content_hash = FileHash::Sha256(hasher.finish().as_ref().try_into().unwrap());
                context
                    .move_to_cache_prehashed(cache_file, content_hash.as_ref(), executable)
                    .await?;

                depmap.insert(
                    filename.as_ref(),
                    FileEntry::Regular {
                        content_hash,
                        executable,
                    }
                    .as_ref(),
                );
            }
            async_tar::EntryType::Directory => {
                depmap.insert(filename.as_ref(), FileEntryRef::Directory);
            }
            async_tar::EntryType::Symlink => {
                let link_name = entry
                    .header()
                    .link_name()?
                    .ok_or_else(|| anyhow!("missing link name for symlink in tar archive"))?;
                let link_name: String = OsString::from(link_name.into_owned())
                    .into_string()
                    .map_err(|_| anyhow!("invalid utf8 in archive filename"))?;
                depmap.insert(filename.as_ref(), FileEntryRef::Symlink(&link_name));
            }
            _ => todo!(),
        }
    }

    let depmap_reference = context.register_concrete_filetree_depmap(depmap.build()).await?;

    Ok(ActionOutput {
        files: depmap_reference,
        stdout: None,
        stderr: None,
    })
}

async fn zip_extract<C: Context, R: AsyncRead>(
    context: &C,
    action: &Extract<ConcreteFiletreeType>,
    mut stream: Pin<&mut R>,
) -> anyhow::Result<ActionOutput> {
    // FIXME: use async here
    let mut archive_bytes = Vec::new();
    stream.read_to_end(&mut archive_bytes).await?;

    let mut depmap = ConcreteFiletree::builder();

    let mut reader = zip::read::ZipArchive::new(std::io::Cursor::new(archive_bytes))?;
    for entry_index in 0..reader.len() {
        let mut entry_bytes = Vec::new();
        let filename;
        let is_file;
        let is_dir;
        let unix_mode;
        {
            let mut entry = reader.by_index(entry_index)?;
            let a_filename = LabelPath::new(entry.name()).context("invalid label path in zip archive")?;

            let a_filename = a_filename
                .normalize_require_descending()
                .context("path within zip archive escapes root")?;

            filename = if let Some(strip_prefix) = &action.strip_prefix {
                let Some(remaining_path) = a_filename.strip_prefix(strip_prefix) else  {
                continue;
            };
                remaining_path.to_owned()
            } else {
                a_filename.into_owned()
            };

            // FIXME: don't buffer like this
            entry.read_to_end(&mut entry_bytes)?;
            is_file = entry.is_file();
            is_dir = entry.is_dir();
            unix_mode = entry.unix_mode();
        }

        if is_file {
            let executable = unix_mode.map(|x| x & 0o100 != 0).unwrap_or(false);
            let mut cache_file = context.tempfile("archive-file", executable).await?;
            let mut hasher = ring::digest::Context::new(&SHA256);

            {
                let cache_file = cache_file.ensure_open().await?;
                hasher.update(&entry_bytes);
                cache_file.write_all_mono(&mut entry_bytes).await?;
            }

            let content_hash = FileHash::Sha256(hasher.finish().as_ref().try_into().unwrap());
            context
                .move_to_cache_prehashed(cache_file, content_hash.as_ref(), executable)
                .await?;

            depmap.insert(
                filename.as_ref(),
                FileEntry::Regular {
                    content_hash,
                    executable,
                }
                .as_ref(),
            );
        } else if is_dir {
            depmap.insert(filename.as_ref(), FileEntryRef::Directory);
        } else {
            todo!()
        }
    }

    let depmap_reference = context.register_concrete_filetree_depmap(depmap.build()).await?;

    Ok(ActionOutput {
        files: depmap_reference,
        stdout: None,
        stderr: None,
    })
}

struct LeadingStream<'a, R> {
    leading: Cursor<Vec<u8>>,
    remaining: Pin<&'a mut R>,
}

impl<'a, R> Read for LeadingStream<'a, R>
where
    Pin<&'a mut R>: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        todo!()
    }
}

impl<'a, 'b, R> Read for Pin<&'b mut LeadingStream<'a, R>>
where
    Pin<&'a mut R>: Read,
{
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        todo!()
    }
}

impl<'a, R: tokio::io::AsyncRead> AsyncRead for LeadingStream<'a, R> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        let this = self.get_mut();
        match std::io::Read::read(&mut this.leading, buf) {
            Ok(0) => {
                let mut read_buf = tokio::io::ReadBuf::new(buf);
                match tokio::io::AsyncRead::poll_read(this.remaining.as_mut(), cx, &mut read_buf) {
                    Poll::Ready(Ok(())) => {
                        let len = read_buf.filled().len();
                        Poll::Ready(Ok(len))
                    }
                    Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
                    Poll::Pending => Poll::Pending,
                }
            }
            Ok(n) => Poll::Ready(Ok(n)),
            Err(err) => Poll::Ready(Err(err)),
        }
    }
}

impl<'a, R: tokio::io::AsyncRead> tokio::io::AsyncRead for LeadingStream<'a, R> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        todo!()
    }
}
