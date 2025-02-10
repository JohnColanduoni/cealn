mod request;

use std::{
    collections::BTreeMap,
    convert::{Infallible, TryFrom},
    fs, io,
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    time::Instant,
};

use anyhow::{bail, Context as AnyhowContext};
use async_lock::Lock;
use cealn_data::{
    action::{Executable, ExecutePlatform, LinuxExecutePlatform},
    depmap::{ConcreteDepmapReference, ConcreteFiletreeType, LabelFiletreeType},
    reference::Reference,
    workspace::GlobalDefaultProvider,
    Label, LabelBuf,
};
use cealn_event::EventContext;
use futures::{channel::mpsc, future::BoxFuture, pin_mut, prelude::*};
use hyper::{service::make_service_fn, Server as HttpServer};
use thiserror::Error;
use tonic::{Request, Response, Status};
use tracing::{debug, error, info, Instrument};

use cealn_core::{trace_call_result, tracing::error_value};
use cealn_protocol::{
    event::grpc::BuildEvent,
    workspace_builder::{
        self,
        grpc::{
            server::{WorkspaceBuilder as WorkspaceBuilderService, WorkspaceBuilderServer},
            BuildRequest, RunRequest, ServerStatus, ServerStatusRequest, StopRequest, StopResponse,
        },
    },
    ServerContext,
};

use crate::{builder::WorkspaceBuilder, graph};

pub struct Server {
    server: BoxFuture<'static, hyper::Result<()>>,
    listen_addr: SocketAddr,
    server_context: ServerContext,
}

struct Service {
    instance: Arc<WorkspaceBuilder>,
    shutdown_signal: Lock<Option<futures::channel::oneshot::Sender<()>>>,
}

impl Server {
    #[tracing::instrument(level = "info", err, skip(instance))]
    pub async fn bind(instance: WorkspaceBuilder, addr: SocketAddr) -> Result<Self, BindError> {
        let (shutdown_signal, shutdown_signal_rx) = futures::channel::oneshot::channel();

        let server_context = instance.server_context().clone();
        let service = WorkspaceBuilderServer::new(Service {
            instance: instance.into(),
            shutdown_signal: Lock::new(Some(shutdown_signal)),
        });

        let server = HttpServer::try_bind(&addr)?
            .http2_only(true)
            // FIXME: shutdown signal
            .serve(make_service_fn(move |_| {
                let service = service.clone();
                async move { Ok::<_, Infallible>(service) }
            }));
        let listen_addr = server.local_addr();

        info!(code = "api_server_bound", %listen_addr);

        trace_call_result!(fs::write(
            server_context.api_url_file_path(),
            format!("http://{}", server.local_addr())
        ))?;

        let server = server.with_graceful_shutdown(shutdown_signal_rx.unwrap_or_else(|err| {
            error!(code = "shutdown_signal_recv_failure", error = error_value(&err));
        }));

        Ok(Server {
            listen_addr,
            server: server.boxed(),
            server_context,
        })
    }

    #[tracing::instrument(level = "info", err, skip(self))]
    pub async fn run(self) -> Result<(), RunError> {
        self.server.await?;
        Ok(())
    }

    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    pub fn server_context(&self) -> &ServerContext {
        &self.server_context
    }

    #[tracing::instrument(level = "debug", err)]
    fn write_api_url_file(server_context: &ServerContext, listen_addr: &SocketAddr) -> Result<(), io::Error> {
        let api_url = format!("http://{}", listen_addr);

        trace_call_result!(fs::write(server_context.api_url_file_path(), &api_url))?;

        debug!(code = "api_url_written", %api_url);

        Ok(())
    }
}

#[tonic::async_trait]
impl WorkspaceBuilderService for Service {
    async fn status(&self, _request: Request<ServerStatusRequest>) -> Result<Response<ServerStatus>, Status> {
        Ok(Response::new(
            workspace_builder::ServerStatus {
                server_executable_mtime: self.instance.server_executable_mtime().clone(),
                launch_environment_variables: self.instance.launch_environment_variables().clone(),
            }
            .into(),
        ))
    }

    async fn stop(&self, _request: Request<StopRequest>) -> Result<Response<StopResponse>, Status> {
        let mut shutdown_signal = self.shutdown_signal.lock().await;
        let shutdown_signal = shutdown_signal
            .take()
            .ok_or_else(|| Status::failed_precondition("shutdown already signaled"))?;

        debug!(code = "trigger_shutdown_signal");
        shutdown_signal
            .send(())
            .map_err(|_err| Status::internal(format!("shutdown signal receiver dropped")))?;

        Ok(Response::new(StopResponse {}))
    }

    type BuildStream = Pin<Box<dyn Stream<Item = Result<BuildEvent, Status>> + Send + 'static>>;
    type RunStream = Pin<Box<dyn Stream<Item = Result<BuildEvent, Status>> + Send + 'static>>;

    async fn build(&self, request: Request<BuildRequest>) -> Result<Response<Self::BuildStream>, Status> {
        let (events_tx, events_rx) = EventContext::new();

        // Treat build future as stream so we can drive it while streaming events from the context
        let instance = self.instance.clone();
        let build = async move {
            // FIXME: distinguish between normal errors and ones that should terminate the stream here, this ignores all
            let _ = instance
                .build(
                    workspace_builder::BuildRequest::try_from(request.into_inner())
                        .map_err(|err| Status::invalid_argument(err.to_string()))?,
                    events_tx,
                )
                .await;

            Ok::<(), anyhow::Error>(())
        };
        let build_as_stream = futures::stream::once(build).filter_map(|result| async move {
            match result {
                Ok(_) => None,
                Err(err) => Some(Err(Status::internal(err.to_string()))),
            }
        });

        let events = events_rx.map(|event| Ok(cealn_protocol::event::grpc::BuildEvent::from(event)));

        Ok(Response::new(
            Box::pin(futures::stream::select(build_as_stream, events)) as Self::BuildStream,
        ))
    }

    async fn run(&self, request: Request<tonic::Streaming<RunRequest>>) -> Result<Response<Self::BuildStream>, Status> {
        let (mut events_tx, events_rx) = EventContext::new();

        let mut input_stream = request.into_inner();

        // Treat build future as stream so we can drive it while streaming events from the context
        let instance = self.instance.clone();
        let build = async move {
            pin_mut!(input_stream);
            let Some(run_request) = input_stream.as_mut().try_next().await? else {
                todo!()
            };
            let run_request: workspace_builder::RunRequest = run_request
                .try_into()
                .map_err(|err: cealn_protocol::ParseError| Status::invalid_argument(err.to_string()))?;

            let analysis = instance
                .analyze(
                    workspace_builder::AnalyzeRequest {
                        target: run_request.target.clone(),
                        default_package: run_request.default_package.clone(),
                        build_config: run_request.build_config.clone(),
                    },
                    events_tx.fork(),
                )
                .await?;

            let Some(executable) = analysis.analysis.providers.iter().find_map(|provider| {
                if &*provider.reference.source_label != Label::new("@com.cealn.builtin//:exec.py").unwrap() || provider.reference.qualname != "Executable" {
                    return None;
                }
                let executable: Executable<LabelFiletreeType> = serde_json::from_str(&serde_json::to_string(provider).unwrap()).unwrap();
                if executable.name.as_deref().map(|x| x == &*run_request.executable_name).unwrap_or(false) {
                    Some(executable)
                } else {
                    None
                }
            }) else {
                bail!("could not find named executable");
            };

            // Load execution platform
            // FIXME: detect
            let provider_ref = Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:exec.py").unwrap(),
                qualname: "LinuxExecutePlatform".to_owned(),
            };
            let request_time = Instant::now();
            let workspace_result = instance
                .graph
                .query_load_root_workspace(request_time, events_tx.fork())
                .await;
            let workspace_result = workspace_result.output_ref()?;
            let mut provider_target = None;
            for supplied_provider in &workspace_result.global_default_providers {
                match supplied_provider {
                    GlobalDefaultProvider::Static {
                        provider_type,
                        providing_target,
                    } => {
                        // TODO: any canonicalization?
                        if provider_type == &provider_ref {
                            provider_target = Some(providing_target.to_owned());
                            break;
                        }
                    }
                }
            }

            let provider_target = if let Some(label) = provider_target {
                label
            } else {
                todo!()
            };

            let provider_result = instance
                .analyze(
                    workspace_builder::AnalyzeRequest {
                        target: provider_target.clone(),
                        default_package: run_request.default_package.clone(),
                        build_config: run_request.build_config.clone(),
                    },
                    events_tx.fork(),
                )
                .await?;

            let provider = provider_result
                .analysis
                .providers
                .iter()
                .find(|provider| &provider.reference == &provider_ref)
                .with_context(|| {
                    format!(
                        "target {} did not supply provider {}",
                        provider_target, provider_ref.qualname
                    )
                })?;

            let platform: ExecutePlatform<LabelFiletreeType> =
                serde_json::from_str(&serde_json::to_string(provider).unwrap()).unwrap();

            let mut source_depmaps = Vec::new();
            source_depmaps.extend(executable.context.clone());
            match &platform {
                ExecutePlatform::Linux(linux) => source_depmaps.push(linux.execution_sysroot.clone()),
                ExecutePlatform::MacOS(_) => todo!(),
            }

            let built_source_depmaps = instance
                .build(
                    workspace_builder::BuildRequest {
                        targets: source_depmaps.clone(),
                        default_package: run_request.default_package,
                        build_config: run_request.build_config,
                        keep_going: false,
                        watch: false,
                    },
                    events_tx.fork(),
                )
                .await?;

            let source_depmap_resolutions: BTreeMap<LabelBuf, ConcreteDepmapReference> = source_depmaps
                .into_iter()
                .zip(built_source_depmaps.into_iter())
                .map(|(k, v)| (k, v.reference.unwrap_or_else(|| todo!())))
                .collect();

            let concrete_executable = Executable::<ConcreteFiletreeType> {
                name: executable.name.clone(),
                executable_path: executable.executable_path.clone(),
                context: executable
                    .context
                    .as_ref()
                    .map(|x| source_depmap_resolutions.get(x).unwrap())
                    .cloned(),
                search_paths: executable.search_paths.clone(),
                library_search_paths: executable.library_search_paths.clone(),
            };
            let concrete_platform = match &platform {
                ExecutePlatform::Linux(platform) => ExecutePlatform::Linux(LinuxExecutePlatform {
                    execution_sysroot: source_depmap_resolutions
                        .get(&platform.execution_sysroot)
                        .unwrap()
                        .clone(),
                    execution_sysroot_input_dest: platform.execution_sysroot_input_dest.clone(),
                    execution_sysroot_output_dest: platform.execution_sysroot_output_dest.clone(),
                    execution_sysroot_exec_context_dest: platform.execution_sysroot_exec_context_dest.clone(),
                    uid: platform.uid,
                    gid: platform.gid,
                    standard_environment_variables: platform.standard_environment_variables.clone(),
                    use_fuse: platform.use_fuse,
                    use_interceptor: platform.use_interceptor,
                }),
                ExecutePlatform::MacOS(_) => todo!(),
            };

            let context = graph::action::Context::new(instance.graph.0.clone(), events_tx.fork());

            let prepared_run = instance
                .graph
                .0
                .executor
                .spawn_immediate({
                    let source_root = instance.server_context.canonical_workspace_root.clone();
                    async move {
                        cealn_action_executable::prepare_for_run(
                            &context,
                            &concrete_executable,
                            &concrete_platform,
                            &source_root,
                        )
                        .await
                    }
                })
                .await?;

            // FIXME: replace all variables
            let executable_path = match &platform {
                ExecutePlatform::Linux(platform) => executable
                    .executable_path
                    .replace("%[execdir]", &platform.execution_sysroot_exec_context_dest),
                ExecutePlatform::MacOS(_) => todo!(),
            };

            events_tx.send(cealn_event::BuildEventData::ExecutablePrepped {
                executable_path,
                parent_pid: prepared_run.parent_pid(),
            });

            Ok(())
        };
        let build_as_stream = futures::stream::once(build).filter_map(|result| async move {
            match result {
                Ok(()) => None,
                Err(err) => Some(Err(Status::internal(err.to_string()))),
            }
        });

        let events = events_rx.map(|event| Ok(cealn_protocol::event::grpc::BuildEvent::from(event)));

        Ok(Response::new(
            Box::pin(futures::stream::select(build_as_stream, events)) as Self::BuildStream,
        ))
    }
}

#[derive(Error, Debug)]
pub enum BindError {
    #[error("IO error encountered while starting server: {0}")]
    Io(#[from] io::Error),
    #[error("failed to bind HTTP server: {0}")]
    Http(#[from] hyper::Error),
}

#[derive(Error, Debug)]
pub enum RunError {
    #[error("fatal HTTP transport error while running server: {0}")]
    Http(#[from] hyper::Error),
}
