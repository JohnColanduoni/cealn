use std::{
    fs::File,
    io::{BufWriter, Write},
    path::PathBuf,
};

use clap::{Parser, Subcommand};
use oci_spec::image::{ImageConfiguration, ImageIndex, ImageManifest, ImageManifestBuilder, MediaType, ToDockerV2S2};
use regex::Regex;
use reqwest::{blocking::Response, header::WWW_AUTHENTICATE, StatusCode};
use serde::{Deserialize, Serialize};

#[derive(Parser)]
struct Opts {
    #[clap(subcommand)]
    sub_command: SubCommand,
}

#[derive(Subcommand)]
enum SubCommand {
    Metadata(MetadataOpts),
    Blob(BlobOpts),
}

#[derive(Parser, Debug)]
pub struct MetadataOpts {
    #[clap(long = "output", required = true)]
    output_path: PathBuf,

    #[clap(long, required = true)]
    os: String,

    #[clap(long, required = true)]
    architecture: String,

    #[clap(name = "IMAGE", required = true)]
    image: String,
}

#[derive(Parser, Debug)]
pub struct BlobOpts {
    #[clap(long = "output", required = true)]
    output_path: PathBuf,

    #[clap(name = "IMAGE", required = true)]
    image: String,

    #[clap(name = "DIGEST", required = true)]
    digest: String,
}

#[derive(Serialize, Deserialize)]
struct Metadata {
    layers: Vec<LayerMetadata>,
    run_config: Option<oci_spec::image::Config>,
}

#[derive(Serialize, Deserialize)]
struct LayerMetadata {
    digest: String,
    diff_id: String,
    media_type: String,
}

fn main() {
    let opts = Opts::parse();

    match opts.sub_command {
        SubCommand::Metadata(metadata_opts) => metadata(metadata_opts),
        SubCommand::Blob(blob_opts) => blob(blob_opts),
    }
}

fn metadata(metadata_opts: MetadataOpts) {
    let image_reference = ImageReference::parse(&metadata_opts.image);
    let client = reqwest::blocking::Client::new();
    let mut token = None;
    let request = client
        .get(format!(
            "https://{}/v2/{}/manifests/{}",
            image_reference.registry,
            image_reference.name,
            image_reference.reference()
        ))
        .header(
            "Accept",
            vec![
                MediaType::ImageIndex.to_string(),
                MediaType::ImageIndex.to_docker_v2s2().unwrap().to_owned(),
                MediaType::ImageManifest.to_string(),
                MediaType::ImageManifest.to_docker_v2s2().unwrap().to_owned(),
            ]
            .into_iter()
            .collect::<Vec<String>>()
            .join(","),
        );

    let response = authenticated_request(&client, &mut token, request);
    let manifest = match MediaType::from(
        response
            .headers()
            .get("content-type")
            .expect("missing content type in manifest response")
            .to_str()
            .unwrap(),
    ) {
        MediaType::ImageManifest => ImageManifest::from_reader(response).unwrap(),
        MediaType::Other(ref other) if other == "application/vnd.docker.distribution.manifest.v2+json" => {
            ImageManifest::from_reader(response).unwrap()
        }
        MediaType::ImageIndex => {
            extract_manifest_from_list(&metadata_opts, &client, &image_reference, &mut token, response)
        }
        MediaType::Other(ref other) if other == "application/vnd.docker.distribution.manifest.list.v2+json" => {
            extract_manifest_from_list(&metadata_opts, &client, &image_reference, &mut token, response)
        }
        other => panic!("invalid manifest response type: {:?}", other),
    };
    let manifest_config: ImageConfiguration =
        get_blob(&client, &image_reference, &mut token, manifest.config().digest())
            .json()
            .unwrap();

    let run_config = manifest_config.config().clone();

    let mut metadata = Metadata {
        layers: Vec::new(),
        run_config,
    };
    for (layer, diff_id) in manifest.layers().iter().zip(manifest_config.rootfs().diff_ids()) {
        metadata.layers.push(LayerMetadata {
            digest: layer.digest().to_string(),
            diff_id: diff_id.to_string(),
            media_type: layer.media_type().to_string(),
        });
    }

    let mut output = BufWriter::new(File::create(&metadata_opts.output_path).unwrap());
    serde_json::to_writer(&mut output, &metadata).unwrap();
    output.flush().unwrap();
}

fn blob(blob_opts: BlobOpts) {
    let image_reference = ImageReference::parse(&blob_opts.image);
    let client = reqwest::blocking::Client::new();
    let mut token = None;
    let mut response = get_blob(&client, &image_reference, &mut token, &blob_opts.digest);
    let mut file = File::create(&blob_opts.output_path).unwrap();
    response.copy_to(&mut file).unwrap();
    file.flush().unwrap();
}

fn extract_manifest_from_list(
    metadata_opts: &MetadataOpts,
    client: &reqwest::blocking::Client,
    image_reference: &ImageReference,
    token: &mut Option<String>,
    response: Response,
) -> ImageManifest {
    let list = ImageIndex::from_reader(response).unwrap();
    for manifest in list.manifests() {
        let Some(platform) = manifest.platform() else {
            continue
        };
        if platform.os().to_string() == metadata_opts.os
            && platform.architecture().to_string() == metadata_opts.architecture
        {
            return get_concrete_manifest(client, image_reference, token, manifest.digest());
        }
    }
    panic!("no matching image for target architecture");
}

fn get_concrete_manifest(
    client: &reqwest::blocking::Client,
    image_reference: &ImageReference,
    token: &mut Option<String>,
    digest: &str,
) -> ImageManifest {
    let request = client
        .get(format!(
            "https://{}/v2/{}/manifests/{}",
            image_reference.registry, image_reference.name, digest
        ))
        .header(
            "Accept",
            vec![
                MediaType::ImageManifest.to_string(),
                MediaType::ImageManifest.to_docker_v2s2().unwrap().to_owned(),
            ]
            .into_iter()
            .collect::<Vec<String>>()
            .join(","),
        );

    let response = authenticated_request(client, token, request);
    ImageManifest::from_reader(response).unwrap()
}

fn get_blob(
    client: &reqwest::blocking::Client,
    image_reference: &ImageReference,
    token: &mut Option<String>,
    digest: &str,
) -> Response {
    let request = client
        .get(format!(
            "https://{}/v2/{}/blobs/{}",
            image_reference.registry, image_reference.name, digest
        ))
        .header(
            "Accept",
            [MediaType::ImageLayer, MediaType::ImageLayerGzip, MediaType::ImageConfig]
                .iter()
                .map(|x| x.to_string())
                .collect::<Vec<_>>()
                .join(","),
        );

    authenticated_request(client, token, request)
}

fn authenticated_request(
    client: &reqwest::blocking::Client,
    token: &mut Option<String>,
    request: reqwest::blocking::RequestBuilder,
) -> Response {
    let mut initial_request = request.try_clone().unwrap();
    if let Some(token) = token.as_deref() {
        initial_request = initial_request.bearer_auth(token);
    }
    let response = initial_request.send().unwrap();
    if response.status() == StatusCode::UNAUTHORIZED {
        // Use WWW-Authenticate to figure out authentication endpoint
        let www_authenticate = response
            .headers()
            .get(WWW_AUTHENTICATE)
            .expect("received 401 from docker registry, but no WWW-Authenticate header was provided");
        let www_authenticate = www_authenticate
            .to_str()
            .expect("failed to parse WWW-Authenticate header");

        let captures = AUTHENTICATE_HEADER_REGEX
            .captures(www_authenticate)
            .expect("failed to parse WWW-Authenticate header");
        let realm = &captures[1];
        let service = &captures[2];
        let scope = &captures[3];

        let auth_request = client.get(realm).query(&[("service", service), ("scope", scope)]);

        let auth_response = auth_request.send().unwrap().error_for_status().unwrap();

        let token_response: TokenResponse = auth_response.json().unwrap();

        let token = token.insert(token_response.token);

        request
            .try_clone()
            .unwrap()
            .bearer_auth(token)
            .send()
            .unwrap()
            .error_for_status()
            .unwrap()
    } else {
        response.error_for_status().unwrap()
    }
}

// TODO: parse this better, this will reject some valid values
lazy_static::lazy_static! {
    static ref AUTHENTICATE_HEADER_REGEX: Regex = Regex::new(r#"^Bearer\s+realm="([^"]+)",service="([^"]+)",scope="([^"]+)"$"#).unwrap();
}

#[derive(Deserialize, Debug)]
struct TokenResponse {
    token: String,
}

struct ImageReference {
    registry: String,
    name: String,
    tag: Option<String>,
    digest: Option<String>,
}

const DEFAULT_REGISTRY: &str = "registry-1.docker.io";

impl ImageReference {
    fn parse(image_name: &str) -> ImageReference {
        let mut components = image_name.split('/').peekable();
        let first_component = components.peek().cloned().unwrap();
        let mut implicit_registry = false;
        let registry = if first_component.contains('@') {
            components.next().unwrap();
            if !components.peek().is_none() {
                panic!("invalid image reference: slash not allowed after digest separator (@)");
            }
            // Single component image with digest
            let (name_and_maybe_tag, digest) = first_component.split_once('@').unwrap();
            if let Some((name, tag)) = name_and_maybe_tag.split_once(':') {
                return ImageReference {
                    registry: DEFAULT_REGISTRY.to_owned(),
                    name: format!("library/{}", name),
                    tag: Some(tag.to_owned()),
                    digest: Some(digest.to_owned()),
                };
            } else {
                return ImageReference {
                    registry: DEFAULT_REGISTRY.to_owned(),
                    name: format!("library/{}", name_and_maybe_tag),
                    tag: None,
                    digest: Some(digest.to_owned()),
                };
            }
        } else if first_component.contains(':') {
            components.next().unwrap();
            if components.peek().is_none() {
                // Single component image with tag
                let (name, tag) = first_component.split_once(':').unwrap();
                assert!(!tag.contains('@')); // Should have been handled above
                return ImageReference {
                    registry: DEFAULT_REGISTRY.to_owned(),
                    name: format!("library/{}", name),
                    tag: Some(tag.to_owned()),
                    digest: None,
                };
            } else {
                // Colon in first component with multiple components always indicates registry host
                first_component.to_owned()
            }
        } else if first_component.contains('.') {
            // Dot in first without : indicates a registry name for sure
            components.next().unwrap();
            first_component.to_owned()
        } else {
            implicit_registry = true;
            DEFAULT_REGISTRY.to_owned()
        };

        let mut name = String::new();
        loop {
            let component = components.next().unwrap();
            if components.peek().is_some() {
                if !name.is_empty() {
                    name.push('/');
                }
                name.push_str(component);
                continue;
            } else {
                if name.is_empty() && implicit_registry {
                    name.push_str("library");
                }
                // Last component
                if let Some((name_component_and_maybe_tag, digest)) = component.split_once('@') {
                    if let Some((name_component, tag)) = name_component_and_maybe_tag.split_once(':') {
                        if !name.is_empty() {
                            name.push('/');
                        }
                        name.push_str(name_component);
                        return ImageReference {
                            registry,
                            name,
                            tag: Some(tag.to_owned()),
                            digest: Some(digest.to_owned()),
                        };
                    } else {
                        if !name.is_empty() {
                            name.push('/');
                        }
                        name.push_str(name_component_and_maybe_tag);
                        return ImageReference {
                            registry,
                            name,
                            tag: None,
                            digest: Some(digest.to_owned()),
                        };
                    }
                } else if let Some((name_component, tag)) = component.split_once(':') {
                    if !name.is_empty() {
                        name.push('/');
                    }
                    name.push_str(name_component);
                    return ImageReference {
                        registry,
                        name,
                        tag: Some(tag.to_owned()),
                        digest: None,
                    };
                } else {
                    if !name.is_empty() {
                        name.push('/');
                    }
                    name.push_str(component);
                    return ImageReference {
                        registry,
                        name,
                        tag: None,
                        digest: None,
                    };
                }
            }
        }
    }

    fn reference(&self) -> &str {
        self.digest.as_deref().or(self.tag.as_deref()).unwrap_or("latest")
    }
}
