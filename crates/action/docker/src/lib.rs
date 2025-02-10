mod config;

use std::{
    collections::HashMap,
    convert::TryInto,
    io::{Read, Seek, SeekFrom, Write},
    str::FromStr,
    sync::Mutex,
};

use anyhow::{anyhow, bail, Context as AnyhowContext};
use async_compression::tokio::bufread::GzipDecoder;
use cealn_depset::{
    depmap::{self, DepMap},
    ConcreteFiletree,
};
use compio_core::{buffer::AllowTake, io::AsyncRead};
use dkregistry::reference::{Reference, Version};
use futures::{pin_mut, prelude::*};
use regex::Regex;
use serde::Deserialize;
use tokio::io::AsyncReadExt;

use cealn_action_context::{
    reqwest::{self, header::WWW_AUTHENTICATE, StatusCode},
    Context,
};
use cealn_data::{
    action::{ActionOutput, DockerDownload},
    file_entry::{FileEntry, FileEntryRef, FileHash},
    label::LabelPath,
};

use crate::config::DockerConfig;

#[tracing::instrument(level = "info", err, skip(context))]
pub async fn download<C: Context>(context: &C, action: &DockerDownload) -> anyhow::Result<ActionOutput> {
    let _events = context.events().fork();

    let docker_config = DockerConfig::load()?;

    let image_id =
        Reference::from_str(&action.image).with_context(|| format!("invalid docker image name {}", action.image))?;

    let http = context.http_client();

    let mut manifest_response = manifest_request(http, &image_id, None).send().await?;

    // Handle authentication if required
    let mut token = None;
    if manifest_response.status() == StatusCode::UNAUTHORIZED {
        let credential = docker_config.get_credentials(&image_id)?;

        // Use WWW-Authenticate to figure out authentication endpoint
        let www_authenticate = manifest_response.headers().get(WWW_AUTHENTICATE).ok_or_else(|| {
            anyhow!("received 401 from docker registry, but not WWW-Authenticate header was provided")
        })?;
        let www_authenticate = www_authenticate
            .to_str()
            .map_err(|_| anyhow!("failed to parse WWW-Authenticate header: {:?}", www_authenticate))?;

        let captures = AUTHENTICATE_HEADER_REGEX
            .captures(www_authenticate)
            .ok_or_else(|| anyhow!("failed to parse WWW-Authenticate header: {:?}", www_authenticate))?;
        let realm = &captures[1];
        let service = &captures[2];
        let scope = &captures[3];

        let mut auth_request = http.get(realm).query(&[("service", service), ("scope", scope)]);

        if let Some(credential) = &credential {
            auth_request = auth_request.basic_auth(&credential.username, Some(&credential.secret));
        }

        let auth_response = auth_request.send().await?;

        if !auth_response.status().is_success() {
            bail!(
                "received status {} when attempting to authenticate to registry",
                auth_response.status()
            );
        }

        let token_response: TokenResponse = auth_response.json().await?;

        manifest_response = manifest_request(http, &image_id, Some(&token_response.token))
            .send()
            .await?;

        token = Some(token_response.token);
    }

    if !manifest_response.status().is_success() {
        bail!(
            "received status {} when attempting to fetch image manifest",
            manifest_response.status(),
        );
    }

    let manifest_response: ManifestResponse = manifest_response.json().await?;
    tracing::debug!(?manifest_response, "received manifest request response");
    // FIXME: if digest provided, ensure manifest matches

    let manifest = match manifest_response {
        ManifestResponse::Manifest2(manifest) => manifest,
        ManifestResponse::OciManifest(manifest) => Manifest2 {
            layers: manifest.layers,
        },
        ManifestResponse::ManifestList(list) => {
            tracing::debug!("received manifest list, obtaining specific manifest");

            let entry = list
                .manifests
                .iter()
                .find(|manifest| {
                    manifest.platform.os == "linux" && manifest.platform.architecture == action.architecture
                })
                .ok_or_else(|| anyhow!("no image matching architecture found"))?;

            tracing::debug!(digest = ?entry.digest, "resolved manifest from manifest list");

            let version = Version::from_str(&format!("@{}", entry.digest))?;
            let updated_image_id = Reference::new(Some(image_id.registry()), image_id.repository(), Some(version));

            let manifest_response = manifest_request(http, &updated_image_id, token.as_deref())
                .send()
                .await?;

            if !manifest_response.status().is_success() {
                bail!(
                    "received status {} when attempting to fetch architecture specific image manifest",
                    manifest_response.status()
                );
            }

            let manifest: Manifest2 = manifest_response.json().await?;

            tracing::debug!("received specific manifest request response");

            manifest
        }
        ManifestResponse::OciIndex(index) => {
            tracing::debug!("received manifest index, obtaining specific manifest");

            let entry = index
                .manifests
                .iter()
                .find(|manifest| {
                    manifest.platform.os == "linux" && manifest.platform.architecture == action.architecture
                })
                .ok_or_else(|| anyhow!("no image matching architecture found"))?;

            let version = Version::from_str(&format!("@{}", entry.digest))?;
            let updated_image_id = Reference::new(Some(image_id.registry()), image_id.repository(), Some(version));

            let manifest_response = manifest_request(http, &updated_image_id, token.as_deref())
                .send()
                .await?;

            if !manifest_response.status().is_success() {
                bail!(
                    "received status {} when attempting to fetch architecture specific image manifest",
                    manifest_response.status()
                );
            }

            let manifest: OciManifest = manifest_response.json().await?;

            tracing::debug!("received specific manifest request response");

            Manifest2 {
                layers: manifest.layers,
            }
        }
    };

    // Download and extract blobs
    let mut root_depmap = ConcreteFiletree::builder();
    for blob in manifest.layers.iter() {
        let mut layer_depmap = ConcreteFiletree::builder();
        let blob_url = format!(
            "https://{}/v2/{}/blobs/{}",
            image_id.registry(),
            image_id.repository(),
            blob.digest
        );
        tracing::debug!(url = ?blob_url, "making blob request");
        let mut blob_request = http.get(blob_url);

        if let Some(token) = &token {
            blob_request = blob_request.bearer_auth(token);
        }

        let mut blob_response = blob_request.send().await?;

        if !blob_response.status().is_success() {
            bail!(
                "received status {} when attempting to fetch image blob",
                blob_response.status()
            );
        }

        // Stream blob to temporary file
        let mut archive_tempfile = context.tempfile("docker_image_blob", false).await?;
        {
            let archive_tempfile = archive_tempfile.ensure_open().await?;
            let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
            while let Some(chunk) = blob_response.chunk().await? {
                hasher.update(&chunk);
                archive_tempfile.write_all(chunk).await?;
            }
            let expected_blob_sum = format!("sha256:{}", hex::encode(hasher.finish()));
            if expected_blob_sum != blob.digest {
                bail!("checksum of downloaded image layer blob did not match");
            }
        }

        // Extract archive
        let archive_tempfile = archive_tempfile.ensure_open().await?;
        archive_tempfile.seek(SeekFrom::Start(0)).await?;
        let mut archive_contents = Vec::new();
        archive_tempfile.read_to_end(AllowTake(&mut archive_contents)).await?;
        // FIXME: use async read here after we fix a crash in tokio-tar
        let archive_poll_reader = archive_tempfile.pollable_read();
        pin_mut!(archive_poll_reader);
        let mut archive = tokio_tar::Archive::new(GzipDecoder::new(tokio::io::BufReader::new(std::io::Cursor::new(
            archive_contents,
        ))));
        let mut file_buffer = vec![0u8; 128 * 1024];
        // These are needed to resolve hard links
        // TODO: Can we avoid or reduce this somehow? Kind of annoying to have two collections.
        let mut current_archive_hashes = HashMap::new();
        let mut entries = archive.entries()?;
        while let Some(mut entry) = entries.try_next().await? {
            let file_path = entry.header().path_bytes();
            let file_path = std::str::from_utf8(&*file_path).map_err(|_| anyhow!("invalid UTF-8 in tar file path"))?;
            let file_path = LabelPath::new(file_path)
                .with_context(|| format!("invalid label path {:?} in tar file entry", file_path))?;
            let file_path = file_path
                .normalize_require_descending()
                .context("tar archive file escapes root")?;
            let file_path = file_path.into_owned();

            let span = tracing::debug_span!("extract_image_file", image_file_path = ?file_path);
            // FIXME: this only works because we don't await here, but we may switch to async file IO in the future, fix
            // this
            let _guard = span.enter();
            match entry.header().entry_type() {
                tokio_tar::EntryType::Regular => {
                    let mode = entry.header().mode()?;
                    let executable = mode & 0o100 != 0;

                    let mut file_hasher = ring::digest::Context::new(&ring::digest::SHA256);
                    let mut cachefile = context.tempfile("docker_image_file", executable).await?;
                    let file_output = cachefile.ensure_open().await?;
                    loop {
                        let read_count = entry.read(&mut file_buffer).await?;
                        if read_count == 0 {
                            break;
                        }
                        file_buffer.truncate(read_count);
                        file_hasher.update(&file_buffer);
                        file_output.write_all_mono(&mut file_buffer).await?;
                        unsafe {
                            file_buffer.set_len(file_buffer.capacity());
                        }
                    }
                    let file_digest = file_hasher.finish();
                    let content_hash = FileHash::Sha256(file_digest.as_ref().try_into().unwrap());
                    context
                        .move_to_cache_prehashed(cachefile, content_hash.as_ref(), executable)
                        .await?;
                    let map_entry = FileEntry::Regular {
                        content_hash,
                        executable,
                    };
                    current_archive_hashes.insert(file_path.clone(), map_entry.clone());
                    layer_depmap.insert(file_path.as_ref(), map_entry.as_ref());
                }
                tokio_tar::EntryType::Symlink => {
                    let link_name = entry
                        .header()
                        .link_name_bytes()
                        .ok_or_else(|| anyhow!("missing symlink name in tar"))?;
                    let link_name = std::str::from_utf8(&*link_name)
                        .map_err(|_| anyhow!("invalid UTF-8 in tar symlink file path"))?;
                    layer_depmap.insert(file_path.as_ref(), FileEntryRef::Symlink(link_name));
                }
                tokio_tar::EntryType::Directory => {
                    layer_depmap.insert(file_path.as_ref(), FileEntryRef::Directory);
                }
                tokio_tar::EntryType::Link => {
                    // We treat hard links as regular files, since the files will be content hashed and deduplicated
                    let link_name = entry
                        .header()
                        .link_name_bytes()
                        .ok_or_else(|| anyhow!("missing hard link name in tar"))?;
                    let link_name = std::str::from_utf8(&*link_name)
                        .map_err(|_| anyhow!("invalid UTF-8 in tar hard link file path"))?;
                    let map_entry = if let Some(stripped_link_name) = link_name.strip_prefix('/') {
                        let stripped_link_name =
                            LabelPath::new(stripped_link_name).context("invalid label path in hardlink target")?;
                        let stripped_link_name = stripped_link_name
                            .normalize_require_descending()
                            .context("hardlink target escapes root")?
                            .into_owned();
                        // Handle root hard links
                        current_archive_hashes.get(&stripped_link_name)
                    } else {
                        let link_name = LabelPath::new(link_name).context("invalid label path in hardlink target")?;
                        let link_name = link_name
                            .normalize_require_descending()
                            .context("hardlink target escapes root")?
                            .into_owned();
                        current_archive_hashes.get(&link_name)
                    };

                    let map_entry = map_entry.with_context(|| {
                        format!(
                            "found hard link to non-existent entry {:?} -> {:?}",
                            file_path, link_name
                        )
                    })?;
                    layer_depmap.insert(file_path.as_ref(), map_entry.as_ref());
                }
                ty => bail!(
                    "unsupported archive entry type {:?} in docker image blob at {:?}",
                    ty,
                    file_path
                ),
            }
        }

        let layer_depmap = layer_depmap.build();
        root_depmap.merge(LabelPath::new("").unwrap().require_normalized_descending().unwrap(), layer_depmap);
    }
    let depmap = root_depmap.build();
    let depmap = context.register_concrete_filetree_depmap(depmap).await?;

    Ok(ActionOutput {
        files: depmap,

        stdout: None,
        stderr: None,
    })
}

fn manifest_request(http: &reqwest::Client, image_id: &Reference, token: Option<&str>) -> reqwest::RequestBuilder {
    let manifest_url = format!(
        "https://{}/v2/{}/manifests/{}",
        image_id.registry(),
        image_id.repository(),
        image_id.version(),
    );

    let mut request = http
        .get(&manifest_url)
        .header("Accept", "application/vnd.docker.distribution.manifest.v2+json")
        .header("Accept", "application/vnd.docker.distribution.manifest.list.v2+json")
        .header("Accept", "application/vnd.oci.image.manifest.v1+json")
        .header("Accept", "application/vnd.oci.image.index.v1+json");

    if let Some(token) = token {
        request = request.bearer_auth(token);
    }

    request
}

// TODO: parse this better, this will reject some valid values
lazy_static::lazy_static! {
    static ref AUTHENTICATE_HEADER_REGEX: Regex = Regex::new(r#"^Bearer\s+realm="([^"]+)",service="([^"]+)",scope="([^"]+)"$"#).unwrap();
}

#[derive(Deserialize, Debug)]
struct TokenResponse {
    token: String,
}

#[derive(Deserialize, Debug)]
#[serde(tag = "mediaType")]
enum ManifestResponse {
    #[serde(rename = "application/vnd.docker.distribution.manifest.v2+json")]
    Manifest2(Manifest2),
    #[serde(rename = "application/vnd.docker.distribution.manifest.list.v2+json")]
    ManifestList(ManifestList2),
    #[serde(rename = "application/vnd.oci.image.manifest.v1+json")]
    OciManifest(OciManifest),
    #[serde(rename = "application/vnd.oci.image.index.v1+json")]
    OciIndex(OciIndex),
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ManifestList2 {
    manifests: Vec<ManifestList2Entry>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ManifestList2Entry {
    digest: String,
    platform: ManifestPlatform,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ManifestPlatform {
    architecture: String,
    os: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Manifest2 {
    layers: Vec<Layer>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Layer {
    media_type: String,
    digest: String,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct OciManifest {
    layers: Vec<Layer>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct OciIndex {
    manifests: Vec<OciIndexEntry>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct OciIndexEntry {
    digest: String,
    platform: ManifestPlatform,
}
