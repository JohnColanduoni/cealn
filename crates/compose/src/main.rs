#![feature(io_error_more)]
#![feature(let_chains)]

mod volume;

use std::{
    cmp::Ordering,
    collections::{hash_map, BTreeMap, HashMap, HashSet},
    env,
    ffi::OsString,
    fmt::Write,
    fs,
    fs::File,
    io::{self, BufReader, Read, Seek, SeekFrom, Write as IoWrite},
    mem,
    net::Ipv4Addr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process,
    str::FromStr,
    sync::Mutex,
    time::Duration,
};

use anyhow::{bail, Context as _, Result};
use clap::{CommandFactory, Parser, Subcommand};
use convert_case::{Case, Casing};
use futures::{
    pin_mut,
    prelude::*,
    stream::{FuturesOrdered, FuturesUnordered},
    try_join, StreamExt,
};
use k8s_openapi::{
    api::{
        core::v1::{Endpoints, Pod, Service},
        discovery::v1::Endpoint,
    },
    apimachinery::pkg::util::intstr::IntOrString,
};
use kube_client::{
    api::{ListParams, Patch, PatchParams},
    config::KubeConfigOptions,
    core::{DynamicObject, GroupVersionKind, Object},
    discovery::ApiResource,
    Api,
};
use mimalloc::MiMalloc;
use rand::prelude::Distribution;
use reqwest::{header::ACCESS_CONTROL_ALLOW_CREDENTIALS, StatusCode, Url};
use ring::digest::SHA256;
use serde::{
    ser::{SerializeMap, SerializeSeq},
    Deserialize,
};
use target_lexicon::{Architecture, Triple};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tracing::{debug, error};

use cealn_cli_support::{
    console::{self, ComposeEvent, ComposeEventData, ComposeEventSource},
    create_client, logging, triple_build_config, ClientOpts,
};
use cealn_client::{
    BuildEvent, BuildEventData, BuildEventSource, BuildRequest, Client, ClientOptions, RunRequest,
    StructuredMessageLevel,
};
use cealn_core::{
    files::{workspace_file_exists_in, WellKnownFileError},
    trace_call_result,
    tracing::error_value,
};
use cealn_data::{reference::Reference, rule::BuildConfig, Label, LabelBuf};
use cealn_docker::{authenticated_request, Credential, DockerConfig, OciManifest};
use cealn_rules_compose_data::{Image, Manifest, PortForward};
use io::LineWriter;

use crate::console::{Console, ConsoleOptions};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Parser)]
#[clap(name = "cealn-compose", version = "0.0.0")]
struct Opts {
    /// Enable debug level logging
    #[clap(long)]
    debug: bool,

    #[clap(subcommand)]
    sub_command: SubCommand,
}

#[derive(Subcommand)]
enum SubCommand {
    Run(RunOpts),
    Deploy(DeployOpts),
    #[clap(hide = true)]
    ShellAutocomplete(ShellAutocompleteOpts),
}

#[derive(Parser, Debug)]
pub struct RunOpts {
    #[clap(flatten)]
    client: ClientOpts,

    #[clap(flatten)]
    kube: KubeOptions,

    #[clap(flatten)]
    docker: DockerOptions,

    #[clap(flatten)]
    build: BuildOpts,

    #[clap(long, default_value = "error")]
    structured_message_max_level: StructuredMessageLevel,
}

#[derive(Parser, Debug)]
pub struct DeployOpts {
    #[clap(flatten)]
    client: ClientOpts,

    #[clap(flatten)]
    kube: KubeOptions,

    #[clap(flatten)]
    docker: DockerOptions,

    #[clap(flatten)]
    build: BuildOpts,

    #[clap(long, default_value = "warn")]
    structured_message_max_level: StructuredMessageLevel,
}

#[derive(Parser, Debug)]
pub struct BuildOpts {
    #[clap(name = "TARGET", default_value = "//:compose")]
    target: LabelBuf,

    #[clap(long = "target", default_value = "x86_64-unknown-linux-gnu")]
    target_triple: Triple,

    /// Print the events from actions that were loaded from the cache
    #[clap(long)]
    print_cached_output: bool,

    // FIXME: autodetect
    #[clap(long = "compose-output-path", required = true)]
    compose_output_path: PathBuf,
}

#[derive(Clone, Parser, Debug)]
struct KubeOptions {
    #[clap(long)]
    kube_context: Option<String>,

    #[clap(long)]
    kube_apply_force: bool,
}

#[derive(Clone, Parser, Debug)]
struct DockerOptions {
    #[clap(long)]
    docker_registry_override: Option<String>,
}

#[derive(Parser, Debug)]
pub struct ShellAutocompleteOpts {
    shell: clap_complete::Shell,
}

fn main() {
    let opts = Opts::parse();

    let evloop = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(8)
        .build()
        .unwrap();

    // Initialize logging
    let log_guard = {
        let _evloop_guard = evloop.enter();
        logging::init(opts.debug, false)
    };
    if opts.debug {
        if !env::var_os("CEALN_BACKTRACE").is_some() {
            env::set_var("CEALN_BACKTRACE", "1");
        }
    }

    let result = match opts.sub_command {
        SubCommand::Run(run_opts) => evloop.block_on(run(&run_opts)),
        SubCommand::Deploy(deploy_opts) => evloop.block_on(deploy(&deploy_opts)),
        SubCommand::ShellAutocomplete(autocomplete_opts) => evloop.block_on(autocomplete(autocomplete_opts)),
    };

    match result {
        Ok(retval) => {
            mem::drop(log_guard);
            process::exit(retval);
        }
        Err(err) => {
            eprintln!("{:?}", err);
            mem::drop(log_guard);
            process::exit(2);
        }
    }
}

#[tracing::instrument("compose::run", level = "info")]
async fn run(run_opts: &RunOpts) -> anyhow::Result<i32> {
    let mut console = Console::new(ConsoleOptions {
        tty: run_opts.client.should_use_terminal(),
        print_cached_output: run_opts.build.print_cached_output,
        max_level: Some(run_opts.structured_message_max_level),
    });

    let mut run_context = Some(RunContext::new(run_opts).await?);

    let mut client = create_client(&run_opts.client).await?;

    let build_config = triple_build_config(&run_opts.build.target_triple, false);

    let stream = client
        .build(BuildRequest {
            targets: vec![run_opts.build.target.clone()],
            // Default from client settings is fine
            default_package: None,
            build_config,
            keep_going: true,
            watch: true,
        })
        .await?;

    let (mut updating_manifest, updating_manifest_rx) = futures::channel::mpsc::channel(1);

    let combined_stream = futures::stream::select(stream.map_ok(Event::Build), updating_manifest_rx.flatten());
    pin_mut!(combined_stream);

    let mut did_error = false;

    let mut manifest_needs_update = false;

    while let Some(event) = combined_stream.try_next().await? {
        match event {
            Event::Build(event) => {
                console.push_build_event(&event);
                match &event.data {
                    BuildEventData::InternalError(_) => {
                        did_error = true;
                    }
                    BuildEventData::WorkspaceFileNotFound { .. } => {
                        did_error = true;
                    }
                    BuildEventData::WatchRun => {
                        console.clear();
                        did_error = false;
                    }
                    BuildEventData::WatchIdle => {
                        if !did_error {
                            manifest_needs_update = true;
                        } else {
                            console.scroll_to_top();
                        }
                    }
                    _ => {}
                }
            }
            Event::Compose(event) => {
                console.push_compose_event(&event);
            }
            Event::ContextDone(a_run_context) => {
                run_context = Some(a_run_context);
            }
        }

        if manifest_needs_update && let Some(mut run_context) = run_context.take() {
            run_context.base.reload_manifest().await?;
            updating_manifest.send(run_context.on_manifest_update()?).await.unwrap();
            manifest_needs_update = false;
        }
    }

    if did_error {
        return Ok(1);
    }

    Ok(0)
}

#[tracing::instrument("compose::deploy", level = "info")]
async fn deploy(deploy_opts: &DeployOpts) -> anyhow::Result<i32> {
    let mut console = Console::new(ConsoleOptions {
        tty: deploy_opts.client.should_use_terminal(),
        print_cached_output: deploy_opts.build.print_cached_output,
        max_level: Some(deploy_opts.structured_message_max_level),
    });

    let mut client = create_client(&deploy_opts.client).await?;

    let build_config = triple_build_config(&deploy_opts.build.target_triple, true);

    let build_stream = client
        .build(BuildRequest {
            targets: vec![deploy_opts.build.target.clone()],
            // Default from client settings is fine
            default_package: None,
            build_config,
            keep_going: false,
            watch: false,
        })
        .await?;

    pin_mut!(build_stream);

    let mut did_error = false;

    while let Some(event) = build_stream.try_next().await? {
        console.push_build_event(&event);
        match &event.data {
            BuildEventData::InternalError(_) => {
                did_error = true;
            }
            BuildEventData::WorkspaceFileNotFound { .. } => {
                did_error = true;
            }
            _ => {}
        }
    }

    if did_error {
        return Ok(1);
    }

    let mut deploy_context = BaseContext::new(
        &deploy_opts.kube,
        &deploy_opts.docker,
        &deploy_opts.build.compose_output_path,
    )
    .await?;

    let (events_tx, mut events_rx) = futures::channel::mpsc::unbounded();
    deploy_context.events_tx = Some(events_tx);

    let relay_events = async move {
        while let Some(event) = events_rx.next().await {
            match event {
                Event::Compose(event) => {
                    console.push_compose_event(&event);
                }
                _ => unreachable!(),
            }
        }
        Ok::<_, anyhow::Error>(())
    };

    let deploy = async move {
        deploy_context.reload_manifest().await?;

        deploy_context.push_images().await?;

        deploy_context.kube_reconcile().await?;

        Ok::<_, anyhow::Error>(())
    };

    futures::try_join!(relay_events, deploy)?;

    Ok(0)
}

enum Event {
    Build(BuildEvent),
    Compose(ComposeEvent),
    ContextDone(RunContext),
}

struct BaseContext {
    kube_opts: KubeOptions,
    docker_opts: DockerOptions,
    compose_path: PathBuf,
    manifest: Option<Manifest>,

    events_tx: Option<futures::channel::mpsc::UnboundedSender<Event>>,

    kube_client: kube_client::Client,
    docker_config: DockerConfig,
    docker_http_client: reqwest::Client,
    docker_credentials: DockerCredentialStore,
    docker_token_store: DockerTokenStore,
    docker_pushed_images: Mutex<HashSet<String>>,
}

impl BaseContext {
    async fn new(kube_opts: &KubeOptions, docker_opts: &DockerOptions, compose_path: &Path) -> Result<BaseContext> {
        let docker_config = DockerConfig::load()?;
        let kube_client = match &kube_opts.kube_context {
            Some(context) => {
                let mut config_options = KubeConfigOptions::default();
                config_options.context = Some(context.to_owned());
                let config = kube_client::Config::from_kubeconfig(&config_options).await?;
                kube_client::Client::try_from(config)?
            }
            None => kube_client::Client::try_default().await?,
        };
        Ok(BaseContext {
            kube_opts: kube_opts.clone(),
            docker_opts: docker_opts.clone(),
            manifest: None,
            compose_path: compose_path.to_owned(),

            events_tx: None,

            kube_client,
            docker_http_client: reqwest::Client::new(),
            docker_credentials: DockerCredentialStore {
                docker_config: docker_config.clone(),
                credentials: Default::default(),
            },
            docker_config,
            docker_token_store: Default::default(),
            docker_pushed_images: Default::default(),
        })
    }
}

struct RunContext {
    base: BaseContext,

    port_forwards: HashMap<PortForward, tokio::task::JoinHandle<Result<()>>>,
}

impl RunContext {
    async fn new(run_opts: &RunOpts) -> Result<RunContext> {
        let base = BaseContext::new(&run_opts.kube, &run_opts.docker, &run_opts.build.compose_output_path).await?;
        Ok(RunContext {
            base,

            port_forwards: Default::default(),
        })
    }

    fn on_manifest_update(mut self) -> anyhow::Result<impl Stream<Item = anyhow::Result<Event>> + 'static> {
        let (events_tx, events_rx) = futures::channel::mpsc::unbounded::<Event>();
        self.base.events_tx = Some(events_tx);

        let drive = async {
            self.base.push_images().await?;

            self.base.kube_reconcile().await?;

            self.push_volumes().await?;

            self.update_port_forwards().await?;
            mem::drop(self.base.events_tx.take());
            Ok::<_, anyhow::Error>(Event::ContextDone(self))
        };

        let source = ComposeEventSource::Deployment;
        let start_events = futures::stream::iter(vec![Ok(Event::Compose(ComposeEvent {
            source: Some(source.clone()),
            data: ComposeEventData::Start,
        }))]);
        let end_events = futures::stream::iter(vec![Ok(Event::Compose(ComposeEvent {
            source: Some(source.clone()),
            data: ComposeEventData::End,
        }))]);

        Ok(start_events
            .chain(futures::stream::select(futures::stream::once(drive), events_rx.map(Ok)))
            .chain(end_events))
    }
}

impl BaseContext {
    fn push_compose_event(&self, event: ComposeEvent) {
        if let Some(events_tx) = &self.events_tx {
            let _ = events_tx.unbounded_send(Event::Compose(event));
        }
    }

    async fn reload_manifest(&mut self) -> anyhow::Result<()> {
        let mut manifest_file = File::open(self.compose_path.join("manifest.json")).unwrap();
        let manifest: Manifest = serde_json::from_reader(&mut manifest_file).unwrap();
        self.manifest = Some(manifest);
        Ok(())
    }

    fn registry_url(&self, image_id: &cealn_docker::Reference) -> anyhow::Result<(String, Url)> {
        let mut registry = self
            .docker_opts
            .docker_registry_override
            .clone()
            .unwrap_or_else(|| image_id.registry());
        let registry_base = if !(registry.starts_with("http://") || registry.starts_with("https://")) {
            format!("https://{}", registry)
        } else {
            registry.clone()
        };
        let url = Url::parse(&registry_base).with_context(|| format!("invalid registry {}", registry))?;
        Ok((registry, url))
    }

    async fn push_images(&self) -> anyhow::Result<()> {
        let manifest = self.manifest.as_ref().unwrap();
        futures::stream::iter(manifest.images.iter())
            .map(|image| self.push_image(image))
            .buffer_unordered(16)
            .try_collect()
            .await
    }

    async fn push_image(&self, image: &Image) -> anyhow::Result<()> {
        let full_image_with_tag = format!("{}:{}", image.full_name, image.tag);
        if self.docker_pushed_images.lock().unwrap().contains(&full_image_with_tag) {
            return Ok(());
        }

        let event_source = ComposeEventSource::Push {
            image_name: image.name.clone(),
            full_image_name: image.full_name.clone(),
            tag: image.tag.clone(),
        };
        self.push_compose_event(ComposeEvent {
            source: Some(event_source.clone()),
            data: ComposeEventData::Start,
        });

        let image_id = cealn_docker::Reference::from_str(&image.full_name)
            .with_context(|| format!("invalid docker image name {}", image.full_name))?;
        let (registry, registry_url) = self.registry_url(&image_id)?;

        let mut manifest_layers = Vec::new();
        let mut diff_ids = Vec::new();

        for layer in &image.layers {
            let mut blob_file;
            let media_type;
            let digest;
            let diff_id;
            match layer {
                cealn_rules_compose_data::ImageLayer::Blob {
                    filename,
                    digest: a_digest,
                    diff_id: a_diff_id,
                    media_type: a_media_type,
                } => {
                    let filename = self.compose_path.join(filename);
                    blob_file = File::open(&filename)?;
                    digest = a_digest.clone();
                    diff_id = a_diff_id.clone();
                    media_type = serde_json::from_value(serde_json::Value::String(a_media_type.clone())).unwrap();
                }
                cealn_rules_compose_data::ImageLayer::Loose(filename) => {
                    let filename = self.compose_path.join(filename);
                    blob_file = tempfile::tempfile()?;
                    {
                        let mut archive = tar::Builder::new(HashingWriter::new(flate2::write::GzEncoder::new(
                            HashingWriter::new(&mut blob_file),
                            // FIXME: configure
                            flate2::Compression::fast(),
                        )));
                        if filename.exists() {
                            assemble_tar(&mut archive, "", &filename)?;
                        }
                        archive.finish()?;
                        let tar_hasher = archive.into_inner()?;
                        diff_id = format!("sha256:{}", hex::encode(tar_hasher.hasher.finish().as_ref()));
                        let gz_hasher = tar_hasher.inner.finish()?;
                        digest = format!("sha256:{}", hex::encode(gz_hasher.hasher.finish().as_ref()));
                    }
                    blob_file.seek(SeekFrom::Start(0))?;
                    media_type = oci_spec::image::MediaType::ImageLayerGzip;
                }
            };

            let blob_len: usize = blob_file.metadata()?.len().try_into()?;
            if !self.blob_exists(&image_id, &digest).await? {
                let event_source = ComposeEventSource::PushLayer {
                    image_name: image.name.clone(),
                    full_image_name: image.full_name.clone(),
                    tag: image.tag.clone(),
                    digest: digest.clone(),
                };
                self.push_compose_event(ComposeEvent {
                    source: Some(event_source.clone()),
                    data: ComposeEventData::Start,
                });

                // TODO: stream body
                let mut blob = Vec::with_capacity(blob_len);
                blob_file.read_to_end(&mut blob)?;

                self.push_blob(&image_id, blob, blob_len, &digest).await?;

                self.push_compose_event(ComposeEvent {
                    source: Some(event_source.clone()),
                    data: ComposeEventData::End,
                });
            }

            let descriptor = oci_spec::image::DescriptorBuilder::default()
                .media_type(media_type)
                .size(blob_len as i64)
                .digest(digest)
                .build()?;
            manifest_layers.push(descriptor);
            diff_ids.push(diff_id);
        }

        let rootfs = oci_spec::image::RootFsBuilder::default()
            .typ("layers")
            .diff_ids(diff_ids)
            .build()?;

        let manifest_config = oci_spec::image::ImageConfigurationBuilder::default()
            // FIXME: detect
            .architecture("amd64")
            .os("linux")
            .rootfs(rootfs)
            .config(image.run_config.clone().unwrap_or_default())
            .build()?;
        let config_blob = serde_json::to_vec(&manifest_config)?;
        let config_blob_digest = format!(
            "sha256:{}",
            hex::encode(ring::digest::digest(&SHA256, &config_blob).as_ref())
        );
        let config_blob_len = config_blob.len();
        self.push_blob(&image_id, config_blob, config_blob_len, &config_blob_digest)
            .await?;
        let config_layer = oci_spec::image::DescriptorBuilder::default()
            .media_type(oci_spec::image::MediaType::ImageConfig)
            .size(config_blob_len as i64)
            .digest(config_blob_digest)
            .build()?;

        let manifest = oci_spec::image::ImageManifestBuilder::default()
            .schema_version(oci_spec::image::SCHEMA_VERSION)
            .config(config_layer)
            .layers(manifest_layers)
            .build()?;

        self.authenticated_docker_request(
            &registry,
            self.docker_http_client
                .put(registry_url.join(&format!("v2/{}/manifests/{}", image_id.repository(), &image.tag,))?)
                .header("Content-Type", "application/vnd.oci.image.manifest.v1+json")
                .json(&manifest),
        )
        .await?
        .error_for_status()?;

        self.push_compose_event(ComposeEvent {
            source: Some(event_source.clone()),
            data: ComposeEventData::End,
        });

        self.docker_pushed_images.lock().unwrap().insert(full_image_with_tag);

        Ok(())
    }
}

fn assemble_tar<W>(archive: &mut tar::Builder<W>, archive_path: &str, src_path: &Path) -> anyhow::Result<()>
where
    W: std::io::Write,
{
    match src_path.read_dir() {
        Ok(entries) => {
            if !archive_path.is_empty() {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Directory);
                header.set_mode(0o755);
                header.set_mtime(0);
                header.set_size(0);
                archive.append_data(&mut header, archive_path, std::io::Cursor::new(vec![]))?;
            }
            let mut entries = entries.collect::<Result<Vec<_>, _>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let file_name = entry.file_name();
                let file_name = file_name.to_str().context("non-utf8 filename")?;
                let file_archive_path = if !archive_path.is_empty() {
                    format!("{archive_path}/{file_name}")
                } else {
                    file_name.to_owned()
                };
                assemble_tar(archive, &file_archive_path, &src_path.join(file_name))?;
            }
            Ok(())
        }
        Err(ref err) if err.kind() == io::ErrorKind::NotADirectory => {
            let metadata = fs::symlink_metadata(src_path)?;
            if metadata.is_file() {
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Regular);
                let executable = metadata.permissions().mode() & 0o100 != 0;
                header.set_mode(if executable { 0o755 } else { 0o644 });
                header.set_mtime(0);
                header.set_size(metadata.len());
                let input = File::open(src_path)?;
                archive.append_data(&mut header, archive_path, input)?;
                Ok(())
            } else if metadata.is_symlink() {
                let target = fs::read_link(src_path)?;
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(tar::EntryType::Symlink);
                let executable = metadata.permissions().mode() & 0o100 != 0;
                header.set_mode(if executable { 0o755 } else { 0o644 });
                header.set_mtime(0);
                // FIXME: not sure this will be consistent on Windows
                archive.append_link(&mut header, archive_path, &target)?;
                Ok(())
            } else {
                todo!()
            }
        }
        Err(err) => return Err(err.into()),
    }
}

impl BaseContext {
    async fn blob_exists(&self, image_id: &cealn_docker::Reference, digest: &str) -> anyhow::Result<bool> {
        let (registry, registry_url) = self.registry_url(image_id)?;

        let check_blob = self
            .authenticated_docker_request(
                &registry,
                self.docker_http_client.head(registry_url.join(&format!(
                    "v2/{}/blobs/{}",
                    image_id.repository(),
                    digest,
                ))?),
            )
            .await?;
        if check_blob.status().is_success() {
            Ok(true)
        } else if check_blob.status() == StatusCode::NOT_FOUND {
            Ok(false)
        } else {
            check_blob.error_for_status()?;
            unreachable!()
        }
    }

    async fn push_blob<T>(
        &self,
        image_id: &cealn_docker::Reference,
        blob: T,
        blob_len: usize,
        digest: &str,
    ) -> anyhow::Result<()>
    where
        reqwest::Body: From<T>,
    {
        let (registry, registry_url) = self.registry_url(image_id)?;

        let create_upload = self
            .authenticated_docker_request(
                &registry,
                self.docker_http_client
                    .post(registry_url.join(&format!("v2/{}/blobs/uploads/", image_id.repository()))?),
            )
            .await?
            .error_for_status()?;
        let location = create_upload
            .headers()
            .get("Location")
            .context("did not receive location header when creating blob upload")?
            .to_str()?;

        let put_location = if !location.starts_with("https://") && !location.starts_with("http://") {
            registry_url.join(&location)?.to_string()
        } else {
            location.to_owned()
        };
        self.authenticated_docker_request(
            &registry,
            self.docker_http_client
                .put(put_location)
                .query(&[("digest", digest)])
                .header("Content-Type", "application/octet-stream")
                .header("Content-Length", blob_len)
                .body(blob),
        )
        .await?
        .error_for_status()?;

        Ok(())
    }

    async fn kube_reconcile(&self) -> anyhow::Result<()> {
        let manifest = self.manifest.as_ref().unwrap();

        let mut patch_params = PatchParams::default();
        patch_params.field_manager = Some("cealn-compose".to_owned());
        patch_params.force = self.kube_opts.kube_apply_force;
        let mut objects = Vec::new();
        for manifest_filename in &manifest.manifests {
            let full_path = self.compose_path.join(manifest_filename);
            let mut manifest_file = BufReader::new(File::open(&full_path)?);
            for document in serde_yaml::Deserializer::from_reader(&mut manifest_file) {
                let object: DynamicObject = DynamicObject::deserialize(document)?;
                objects.push((manifest_filename.to_owned(), object));
            }
        }

        let mut crd_phase = Vec::new();
        let mut namespace_phase = Vec::new();
        let mut operator_phase = Vec::new();
        let mut general_phase = Vec::new();
        for (manifest_filename, object) in &objects {
            let kind = object.types.as_ref().map(|t| &*t.kind);
            match kind {
                Some("Namespace") => {
                    namespace_phase.push((manifest_filename, object));
                }
                Some("CustomResourceDefinition") => {
                    crd_phase.push((manifest_filename, object));
                }
                _ => match object.metadata.name.as_deref() {
                    Some(s) if s.contains("operator") => {
                        operator_phase.push((manifest_filename, object));
                    }
                    _ => {
                        general_phase.push((manifest_filename, object));
                    }
                },
            }
        }

        for phase in &[&crd_phase, &namespace_phase, &operator_phase, &general_phase] {
            futures::stream::iter(phase.iter())
                .map(Ok)
                .try_for_each_concurrent(Some(16), |(manifest_filename, object)| async {
                    self.kube_reconcile_object(&patch_params, manifest_filename, object)
                        .await
                })
                .await?;
        }

        Ok(())
    }

    async fn kube_reconcile_object(
        &self,
        patch_params: &PatchParams,
        manifest_filename: &str,
        object: &DynamicObject,
    ) -> anyhow::Result<()> {
        let patch = Patch::Apply(&object);
        let name = object
            .metadata
            .name
            .as_deref()
            .with_context(|| format!("missing object name in {:?}", manifest_filename))?;
        let types = object
            .types
            .as_ref()
            .with_context(|| format!("missing type on object {:?}", name))?;
        let api_resource = ApiResource::from_gvk(&GroupVersionKind::try_from(types)?);
        let event_source;
        let api = match object.metadata.namespace.as_deref() {
            Some(namespace) => {
                event_source = ComposeEventSource::Apply {
                    kind: types.kind.clone(),
                    name: name.to_owned(),
                    namespace: Some(namespace.to_owned()),
                };
                Api::<DynamicObject>::namespaced_with(self.kube_client.clone(), &namespace, &api_resource)
            }
            None => {
                event_source = ComposeEventSource::Apply {
                    kind: types.kind.clone(),
                    name: name.to_owned(),
                    namespace: None,
                };
                Api::<DynamicObject>::all_with(self.kube_client.clone(), &api_resource)
            }
        };
        self.push_compose_event(ComposeEvent {
            source: Some(event_source.clone()),
            data: ComposeEventData::Start,
        });
        let mut fetch_attempts_remaining = 16usize;
        let original = loop {
            match api.get_opt(name).await {
                Ok(original) => break original,
                Err(kube_client::Error::Api(ref err)) if err.code == 404 && fetch_attempts_remaining > 0 => {
                    // Likely a missing crd. Wait for it to exist
                    fetch_attempts_remaining -= 1;
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    continue;
                }

                Err(err) => {
                    return Err(anyhow::Error::from(err).context(format!(
                        "failed to fetch {} {} in namespace {}",
                        types.kind,
                        name,
                        object.metadata.namespace.as_deref().unwrap_or("GLOBAL")
                    )))
                }
            }
        };
        let mut update_attempts_remaining = 16usize;
        let updated = loop {
            match api.patch(name, &patch_params, &patch).await {
                Ok(updated) => break updated,
                Err(kube_client::Error::Api(ref err))
                    if err.message.contains("failed calling webhook") && update_attempts_remaining > 0 =>
                {
                    // Could be an admission controller that hasn't been applied yet
                    update_attempts_remaining -= 1;
                    tokio::time::sleep(Duration::from_secs(4)).await;
                    continue;
                }
                Err(err) => {
                    return Err(anyhow::Error::from(err).context(format!(
                        "failed to patch {} {} in namespace {}",
                        types.kind,
                        name,
                        object.metadata.namespace.as_deref().unwrap_or("GLOBAL")
                    )))
                }
            }
        };
        match original {
            Some(original) => {
                let original_spec = original
                    .data
                    .get("spec")
                    .cloned()
                    .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
                let updated_spec = updated
                    .data
                    .get("spec")
                    .cloned()
                    .unwrap_or_else(|| serde_json::Value::Object(Default::default()));
                self.write_diff(&event_source, "spec".to_owned(), &original_spec, &updated_spec);
            }
            None => {
                self.push_compose_event(ComposeEvent {
                    source: Some(event_source.clone()),
                    data: ComposeEventData::NewObject,
                });
            }
        }
        self.push_compose_event(ComposeEvent {
            source: Some(event_source.clone()),
            data: ComposeEventData::End,
        });

        Ok(())
    }

    fn write_diff(
        &self,
        source: &ComposeEventSource,
        leading_field_path: String,
        old: &serde_json::Value,
        new: &serde_json::Value,
    ) {
        match (old, new) {
            (serde_json::Value::Object(old), serde_json::Value::Object(new)) => {
                for (k, new_value) in new {
                    let leading_field_path = format!("{}.{}", leading_field_path, k);
                    match old.get(k) {
                        Some(old_value) => {
                            self.write_diff(source, leading_field_path, old_value, new_value);
                        }
                        None => {
                            // FIXME: notify fields added
                        }
                    }
                }
            }
            (serde_json::Value::Array(old), serde_json::Value::Array(new)) => {
                for (index, new_value) in new.iter().enumerate() {
                    let leading_field_path = format!("{}[{}]", leading_field_path, index);
                    match old.get(index) {
                        Some(old_value) => {
                            self.write_diff(source, leading_field_path, old_value, new_value);
                        }
                        None => {
                            // FIXME: notify fields added
                        }
                    }
                }
            }
            (serde_json::Value::String(old_string), serde_json::Value::String(new_string))
                if old_string != new_string =>
            {
                self.push_compose_event(ComposeEvent {
                    source: Some(source.clone()),
                    data: ComposeEventData::ModifyObjectField {
                        field_path: leading_field_path.clone(),
                        old_value: old.clone(),
                        new_value: new.clone(),
                    },
                });
            }
            // FIXME: implement more
            _ => {}
        }
    }

    async fn authenticated_docker_request(
        &self,
        registry: &str,
        request: reqwest::RequestBuilder,
    ) -> anyhow::Result<reqwest::Response> {
        cealn_docker::authenticated_request(
            &self.docker_http_client,
            &self.docker_credentials,
            &self.docker_token_store,
            registry,
            request,
        )
        .await
    }
}

impl RunContext {
    async fn update_port_forwards(&mut self) -> anyhow::Result<()> {
        let manifest = self.base.manifest.as_ref().unwrap();

        for port_forward in &manifest.port_forwards {
            // FIXME: source errors in port forward tasks
            match self.port_forwards.entry(port_forward.clone()) {
                hash_map::Entry::Occupied(_) => {}
                hash_map::Entry::Vacant(entry) => {
                    entry.insert(tokio::spawn(run_port_forward(
                        self.base.kube_client.clone(),
                        port_forward.clone(),
                    )));
                }
            }
        }

        for port_forward in self.port_forwards.keys().cloned().collect::<Vec<_>>() {
            if !manifest.port_forwards.contains(&port_forward) {
                if let Some(forwarder) = self.port_forwards.remove(&port_forward) {
                    forwarder.abort();
                }
            }
        }

        Ok(())
    }
}

struct DockerCredentialStore {
    docker_config: DockerConfig,
    credentials: tokio::sync::Mutex<HashMap<String, Credential>>,
}

impl<'b> cealn_docker::CredentialSource for &'b DockerCredentialStore {
    fn get<'a>(&'a mut self, registry: &'a str) -> future::BoxFuture<'a, anyhow::Result<Option<Credential>>> {
        async {
            let mut tokens = self.credentials.lock().await;
            match tokens.entry(registry.to_owned()) {
                hash_map::Entry::Occupied(entry) => Ok(Some(entry.get().clone())),
                hash_map::Entry::Vacant(entry) => {
                    let credential = self.docker_config.get_credentials(registry)?;
                    if let Some(credential) = credential {
                        Ok(Some(entry.insert(credential).clone()))
                    } else {
                        Ok(None)
                    }
                }
            }
        }
        .boxed()
    }

    fn refresh<'a>(&'a mut self, registry: &'a str) -> future::BoxFuture<'a, anyhow::Result<Option<Credential>>> {
        async {
            let mut tokens = self.credentials.lock().await;
            let credential = self.docker_config.get_credentials(registry)?;
            if let Some(credential) = credential {
                tokens.insert(registry.to_owned(), credential.clone());
                Ok(Some(credential))
            } else {
                Ok(None)
            }
        }
        .boxed()
    }
}

#[derive(Default)]
struct DockerTokenStore {
    tokens: Mutex<HashMap<(String, String, String), String>>,
}

impl<'a> cealn_docker::TokenSource for &'a DockerTokenStore {
    fn get(&mut self, realm: &str, service: &str, scope: &str) -> Option<String> {
        let tokens = self.tokens.lock().unwrap();
        tokens
            .get(&(realm.to_owned(), service.to_owned(), scope.to_owned()))
            .cloned()
    }

    fn set(&mut self, realm: &str, service: &str, scope: &str, token: &str) {
        let mut tokens = self.tokens.lock().unwrap();
        tokens.insert(
            (realm.to_owned(), service.to_owned(), scope.to_owned()),
            token.to_owned(),
        );
    }
}

async fn run_port_forward(client: kube_client::Client, port_forward: PortForward) -> anyhow::Result<()> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port_forward.local_port)).await?;
    let tasks = FuturesUnordered::new();
    loop {
        // FIXME: handle errors in tasks, cleanup finished tasks
        let (socket, _) = listener.accept().await?;
        tasks.push(tokio::spawn({
            let client = client.clone();
            let port_forward = port_forward.clone();
            async move {
                if let Err(err) = run_port_forward_connection(client, port_forward, socket).await {
                    // FIXME: send through console
                    // FIXME: cleanup task
                    eprintln!("port forwarding failed: {}", err);
                }
            }
        }));
    }
}

async fn run_port_forward_connection(
    client: kube_client::Client,
    port_forward: PortForward,
    socket: TcpStream,
) -> anyhow::Result<()> {
    let namespace;
    let pod_name;
    let pod_port;
    match (&*port_forward.types.api_version, &*port_forward.types.kind) {
        ("core/v1", "Pod") => {
            namespace = port_forward
                .resource
                .namespace
                .as_deref()
                .context("pods require a namespace name")?;
            pod_name = port_forward.resource.name.to_owned();
            pod_port = port_forward.resource_port;
        }
        ("v1", "Service") => {
            namespace = port_forward
                .resource
                .namespace
                .as_deref()
                .context("services require a namespace name")?;
            let services_api: Api<Service> = Api::namespaced(client.clone(), namespace);
            let service = services_api.get(&port_forward.resource.name).await?;
            let target_port = service
                .spec
                .as_ref()
                .context("missing spec")?
                .ports
                .iter()
                .flatten()
                .filter(|port| port.port as u16 == port_forward.resource_port)
                .filter_map(|port| port.target_port.as_ref())
                .next()
                .context("port not found on service")?;
            pod_port = match target_port {
                IntOrString::Int(port) => *port as u16,
                IntOrString::String(_) => todo!(),
            };

            let endpoints_api: Api<Endpoints> = Api::namespaced(client.clone(), namespace);
            let endpoints = endpoints_api
                .get_opt(&port_forward.resource.name)
                .await?
                .context("no endpoints")?;
            let matching_pods = endpoints
                .subsets
                .iter()
                .flatten()
                .filter(|subset| subset.ports.iter().flatten().any(|port| port.port as u16 == pod_port))
                .flat_map(|subset| subset.addresses.iter().flatten())
                .filter_map(|address| address.target_ref.as_ref())
                .filter_map(|target| target.name.as_deref())
                .collect::<Vec<_>>();
            pod_name = rand::distributions::Slice::new(&matching_pods)
                .context("no matching endpoint addresses")?
                .sample(&mut rand::thread_rng())
                .to_string();
        }
        _ => bail!("unsupported resource for port forwarding"),
    }

    let pods: Api<Pod> = Api::namespaced(client.clone(), namespace);
    let mut forwarder = pods.portforward(&pod_name, &[pod_port]).await?;
    let stream = forwarder.take_stream(pod_port).context("missing port forward stream")?;
    run_port_forward_socket(socket, stream).await
}

async fn run_port_forward_socket(
    mut socket: tokio::net::TcpStream,
    mut stream: impl tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
) -> anyhow::Result<()> {
    let mut socket_buffer = vec![0u8; 64 * 1024];
    let mut stream_buffer = vec![0u8; 64 * 1024];
    loop {
        futures::select! {
            result = socket.read(&mut socket_buffer).fuse() => {
                let read_len = match result {
                    Ok(0) => break,
                    Ok(read_len) => read_len,
                    Err(ref err) if err.kind() == io::ErrorKind::ConnectionReset => break,
                    Err(err) => return Err(err.into()),
                };
                stream.write_all(&socket_buffer[..read_len]).await?;
            },
            read_len = stream.read(&mut stream_buffer).fuse() => {
                let read_len = read_len?;
                if read_len == 0 {
                    break;
                }
                socket.write_all(&stream_buffer[..read_len]).await?;
            }
        }
    }

    Ok(())
}

async fn autocomplete(autocomplete_opts: ShellAutocompleteOpts) -> anyhow::Result<i32> {
    clap_complete::generate(
        autocomplete_opts.shell,
        &mut Opts::command(),
        "cealn-compose",
        &mut std::io::stdout(),
    );
    Ok(0)
}

struct HashingWriter<W> {
    inner: W,
    hasher: ring::digest::Context,
}

impl<W: io::Write> HashingWriter<W> {
    fn new(inner: W) -> Self {
        HashingWriter {
            inner,
            hasher: ring::digest::Context::new(&SHA256),
        }
    }
}

impl<W: io::Write> io::Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let write_len = self.inner.write(buf)?;
        self.hasher.update(&buf[..write_len]);
        Ok(write_len)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}
