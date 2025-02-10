#![feature(let_chains)]

mod config;

pub use crate::config::{Credential, DockerConfig};
pub use dkregistry::reference::{Reference, Version};

use std::{
    collections::HashMap,
    convert::TryInto,
    io::{Read, Seek, SeekFrom, Write},
    str::FromStr,
    sync::Mutex,
};

use anyhow::{anyhow, bail, Context as AnyhowContext};
use async_compression::tokio::bufread::GzipDecoder;
use compio_core::{buffer::AllowTake, io::AsyncRead};
use futures::{future::BoxFuture, pin_mut, prelude::*};
use http_auth::ChallengeParser;
use regex::Regex;
use reqwest::{header::WWW_AUTHENTICATE, StatusCode};
use serde::{Deserialize, Serialize};
use tokio::io::AsyncReadExt;

pub async fn authenticated_request<C: CredentialSource, T: TokenSource>(
    http: &reqwest::Client,
    mut credential: C,
    mut token: T,
    registry: &str,
    request: reqwest::RequestBuilder,
) -> anyhow::Result<reqwest::Response> {
    let mut did_try_stored_token = false;
    let mut retry_without_stored_token = false;
    loop {
        let response = request.try_clone().context("unclonable request")?.send().await?;
        if response.status() == StatusCode::UNAUTHORIZED {
            // Use WWW-Authenticate to figure out authentication endpoint
            let www_authenticate = response
                .headers()
                .get(WWW_AUTHENTICATE)
                .context("received 401 from docker registry, but no WWW-Authenticate header was provided")?;
            let www_authenticate = www_authenticate
                .to_str()
                .map_err(|_| anyhow!("failed to parse WWW-Authenticate header: {:?}", www_authenticate))?
                .to_owned();

            let challenges = http_auth::parse_challenges(&*www_authenticate).map_err(|err| anyhow!("{}", err))?;

            if let Some(bearer_challenge) = challenges.iter().find(|challenge| challenge.scheme == "Bearer") {
                let realm = bearer_challenge
                    .params
                    .iter()
                    .find(|(k, _)| *k == "realm")
                    .map(|(_, v)| v)
                    .context("missing realm in OAuth2 challenge")?
                    .to_unescaped();
                let service = bearer_challenge
                    .params
                    .iter()
                    .find(|(k, _)| *k == "service")
                    .map(|(_, v)| v)
                    .context("missing service in OAuth2 challenge")?
                    .to_unescaped();
                let scope = bearer_challenge
                    .params
                    .iter()
                    .find(|(k, _)| *k == "scope")
                    .map(|(_, v)| v)
                    .context("missing service in OAuth2 challenge")?
                    .to_unescaped();

                let token = if let Some(token) = token.get(&realm, &service, &scope) && !retry_without_stored_token {
                    did_try_stored_token = true;
                    token
                } else {
                    let mut is_token_fetch_retry = false;
                    loop {
                        let credential = if !is_token_fetch_retry {
                            credential.get(registry).await?
                        } else {
                            credential.refresh(registry).await?
                        };

                        let mut auth_request = http
                            .get(&realm)
                            .query(&[("service", &*service), ("scope", &*scope)]);

                        if let Some(credential) = &credential {
                            auth_request = auth_request.basic_auth(&credential.username, Some(&credential.secret));
                        }

                        let auth_response = auth_request.send().await?;

                        if auth_response.status() == StatusCode::UNAUTHORIZED && !is_token_fetch_retry {
                            is_token_fetch_retry = true;
                            continue;
                        }

                        if !auth_response.status().is_success() {
                            bail!(
                                "received status {} when attempting to authenticate to registry",
                                auth_response.status()
                            );
                        }

                        let token_response: TokenResponse = auth_response.json().await?;

                        token.set(&realm, &service, &scope, &token_response.token);
                        break token_response.token;
                    }
                };

                let response = request
                    .try_clone()
                    .context("unclonable request")?
                    .bearer_auth(token)
                    .send()
                    .await?;
                if response.status() == StatusCode::UNAUTHORIZED && did_try_stored_token && !retry_without_stored_token
                {
                    // Token may be stale, retry without it
                    retry_without_stored_token = true;
                    continue;
                } else {
                    return Ok(response);
                }
            } else if let Some(_) = challenges.iter().find(|challenge| challenge.scheme == "Basic") {
                let mut is_credential_retry = false;
                loop {
                    let credential = if !is_credential_retry {
                        credential.get(registry).await?
                    } else {
                        credential.refresh(registry).await?
                    };
                    let credential = credential.with_context(|| {
                        format!("no credential available for registry {}, but one is required", registry)
                    })?;

                    let response = request
                        .try_clone()
                        .context("unclonable request")?
                        .basic_auth(&credential.username, Some(&credential.secret))
                        .send()
                        .await?;
                    if response.status() == StatusCode::UNAUTHORIZED && !is_credential_retry {
                        // Credential may be stale, retry without it
                        is_credential_retry = true;
                        continue;
                    } else {
                        return Ok(response);
                    }
                }
            } else {
                bail!("unknown auth challenge type: {:?}", www_authenticate)
            }
        } else {
            return Ok(response);
        }
    }
}

pub trait CredentialSource {
    fn get<'a>(&'a mut self, registry: &'a str) -> BoxFuture<'a, anyhow::Result<Option<Credential>>>;
    fn refresh<'a>(&'a mut self, registry: &'a str) -> BoxFuture<'a, anyhow::Result<Option<Credential>>>;
}

impl<'a> CredentialSource for &'a Credential {
    fn get(&mut self, registry: &str) -> BoxFuture<anyhow::Result<Option<Credential>>> {
        let credential = self.clone();
        async move { Ok(Some(credential)) }.boxed()
    }
    fn refresh(&mut self, registry: &str) -> BoxFuture<anyhow::Result<Option<Credential>>> {
        async move { Err(anyhow!("only static credential provided")) }.boxed()
    }
}

pub trait TokenSource {
    fn get(&mut self, realm: &str, service: &str, scope: &str) -> Option<String>;
    fn set(&mut self, realm: &str, service: &str, scope: &str, token: &str);
}

impl<'a> TokenSource for &'a mut Option<String> {
    fn get(&mut self, realm: &str, service: &str, scope: &str) -> Option<String> {
        self.clone()
    }
    fn set(&mut self, realm: &str, service: &str, scope: &str, token: &str) {
        **self = Some(token.to_owned());
    }
}

pub fn manifest_request(http: &reqwest::Client, image_id: &Reference, token: Option<&str>) -> reqwest::RequestBuilder {
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

#[derive(Deserialize, Debug)]
struct TokenResponse {
    token: String,
}

#[derive(Serialize, Deserialize, Debug)]
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

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ManifestList2 {
    manifests: Vec<ManifestList2Entry>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ManifestList2Entry {
    digest: String,
    platform: ManifestPlatform,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct ManifestPlatform {
    architecture: String,
    os: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Manifest2 {
    pub layers: Vec<Layer>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct Layer {
    pub media_type: String,
    pub digest: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub struct OciManifest {
    pub layers: Vec<Layer>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct OciIndex {
    manifests: Vec<OciIndexEntry>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct OciIndexEntry {
    digest: String,
    platform: ManifestPlatform,
}
