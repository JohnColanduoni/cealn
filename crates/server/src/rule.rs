use std::{
    cell::RefCell,
    collections::BTreeMap,
    mem,
    sync::{Arc, Mutex},
};

use anyhow::{bail, Context as AnyhowContext, Result};
use cealn_depset::{ConcreteFiletree, DepMap};
use cealn_event::EventContext;
use futures::{future::BoxFuture, join, lock::Mutex as AsyncMutex, pin_mut, prelude::*, stream::FuturesOrdered};
use thiserror::Error;

use cealn_data::{
    action::{ActionOutput, LabelAction},
    depmap::{ConcreteDepmapReference, DepmapHash},
    file_entry::{FileEntry, FileEntryRef, FileHash, FileHashRef},
    reference::Reference,
    rule::{BuildConfig, Provider, Target},
    Label, LabelBuf,
};
use cealn_protocol::query::{AnalysisQueryProduct, LoadQueryProduct, StdioLine, StdioStreamType};
use cealn_runtime::{
    api::{types, Api, ApiDispatch, Handle, InjectFdError},
    Instance, Interpreter,
};
use cealn_runtime_data::{
    fs::{NAMED_WORKSPACE_MOUNT_PATH, WORKSPACE_MOUNT_PATH},
    package_load::LoadPackageIn,
    rule::{
        AnalyzeAsyncRequest, AnalyzeEvent, PollAnalyzeTargetIn, PollAnalyzeTargetOut, PrepareRuleIn,
        StartAnalyzeTargetIn,
    },
};

use tracing::{debug, debug_span, info, Instrument};

use crate::{
    executor::Executor,
    runtime::logger::{self, Logger},
};

/// An actively processing load operation for a
pub(crate) struct Analyzer {
    runtime: Instance<LoaderApi>,
    target_label: LabelBuf,
    target: Target,
    build_config: BuildConfig,
    logger: Option<Logger>,
}

// TODO: use type members for the futures returned by these functions
pub(crate) trait Context {
    fn labeled_target_exists<'a>(&'a self, label: &'a Label) -> BoxFuture<'a, anyhow::Result<bool>>;
    fn labeled_file_exists<'a>(&'a self, label: &'a Label) -> BoxFuture<'a, anyhow::Result<bool>>;
    fn labeled_file_is_file<'a>(&'a self, label: &'a Label) -> BoxFuture<'a, anyhow::Result<bool>>;
    fn load_providers<'a>(
        &'a self,
        target: &'a Label,
        build_config: BuildConfig,
    ) -> BoxFuture<'a, anyhow::Result<Vec<Provider>>>;
    fn load_file_label<'a>(
        &'a self,
        label: &'a Label,
    ) -> BoxFuture<'a, anyhow::Result<Option<ConcreteDepmapReference>>>;
    fn load_global_provider<'a>(
        &'a self,
        provider: &'a Reference,
        build_config: BuildConfig,
    ) -> BoxFuture<'a, anyhow::Result<Option<Provider>>>;
    fn run_action<'a>(
        &'a self,
        action: LabelAction,
        partial_actions: BTreeMap<LabelBuf, LabelAction>,
    ) -> BoxFuture<'a, anyhow::Result<ActionOutput>>;
    fn get_filetree<'a>(&'a self, reference: &'a DepmapHash) -> BoxFuture<'a, anyhow::Result<ConcreteFiletree>>;
    fn open_cache_file<'a>(
        &'a self,
        hash: FileHashRef<'a>,
        executable: bool,
    ) -> BoxFuture<'a, anyhow::Result<Arc<dyn cealn_runtime::api::Handle>>>;
}

pub(crate) enum LabeledFileContents {
    SourceFile,
}

impl Analyzer {
    #[tracing::instrument("Analyzer::new", level = "info", err, skip(python_interpreter, named_workspaces_fs))]
    pub(crate) async fn new(
        python_interpreter: &Interpreter,
        events: EventContext,
        named_workspaces_fs: Arc<dyn Handle>,
        target_label: LabelBuf,
        target: Target,
        build_config: BuildConfig,
    ) -> Result<Self> {
        let api = LoaderApi(Arc::new(_LoaderApi {
            filesystems: vec![(NAMED_WORKSPACE_MOUNT_PATH.to_owned(), named_workspaces_fs)],
        }));

        let builder = Instance::builder(python_interpreter, api)?;

        let (logger, stdout_handle, stderr_handle) = Logger::new(events);
        builder
            .wasi_ctx()
            .inject_fd(cealn_runtime_virt::fs::null::new(), Some(types::Fd::from(0)))?;
        builder.wasi_ctx().inject_fd(stdout_handle, Some(types::Fd::from(1)))?;
        builder.wasi_ctx().inject_fd(stderr_handle, Some(types::Fd::from(2)))?;

        let runtime = builder.build().await?;

        Ok(Analyzer {
            runtime,
            target_label,
            target,
            build_config,
            logger: Some(logger),
        })
    }

    #[tracing::instrument("Analyzer::analyze", level = "info", err, skip(self, executor, context), fields(target=%self.target_label))]
    pub(crate) async fn analyze<C: Context>(
        mut self,
        executor: &Executor,
        context: &C,
    ) -> Result<AnalysisQueryProduct> {
        // FIXME: handle blocking better here or switch WASM to async
        self.runtime
            .prepare_rule(&PrepareRuleIn {
                rule: self.target.rule.clone(),
            })
            .await?;

        self.runtime
            .start_analyze_target(&StartAnalyzeTargetIn {
                target: self.target.clone(),
                target_label: self.target_label.clone(),
                build_config: self.build_config.clone(),
            })
            .await?;

        let first_poll = self
            .runtime
            .poll_analyze_target(&PollAnalyzeTargetIn {
                event: AnalyzeEvent::FirstPoll,
            })
            .await?;

        // TODO: see if we can avoid this, we'll only ever access this from one thread at a time
        let this = AsyncMutex::new(self);
        let analysis = match first_poll {
            PollAnalyzeTargetOut::Done(result) => result,
            PollAnalyzeTargetOut::Requests(requests) => {
                // Run all requests in parallel, but to ensure determinism only give them to the runtime in their
                // requested order.
                let mut outstanding_requests = FuturesOrdered::new();
                for request in requests.into_iter() {
                    outstanding_requests.push(Self::issue_request(&this, context, request));
                }
                'resolving_loop: loop {
                    // Drive all requests, and return them to the runtime in order as they complete
                    while let Some(item) = outstanding_requests.try_next().await? {
                        let new_state = {
                            let mut this = this.lock().await;
                            this.runtime
                                .poll_analyze_target(&PollAnalyzeTargetIn { event: item })
                                .await?
                        };
                        match new_state {
                            PollAnalyzeTargetOut::Done(result) => break 'resolving_loop result,
                            PollAnalyzeTargetOut::Requests(new_requests) => {
                                for request in new_requests {
                                    outstanding_requests.push(Self::issue_request(&this, context, request));
                                }
                            }
                        }
                    }
                    bail!("rule analysis has stalled");
                }
            }
        };

        let mut this = this.into_inner();
        let logger = this.logger.take().unwrap();
        let output_mounts = this.target.output_mounts.clone();
        // Drop runtime to allow stdio streams to finish
        mem::drop(this);
        let stdio = logger.finish();

        Ok(AnalysisQueryProduct {
            analysis,
            output_mounts,
            stdio,
        })
    }

    fn issue_request<'a, C: Context>(
        this: &'a AsyncMutex<Self>,
        context: &'a C,
        request: AnalyzeAsyncRequest,
    ) -> impl Future<Output = anyhow::Result<AnalyzeEvent>> + 'a {
        let span = debug_span!("Analyzer::issue_request", ?request);
        async move {
            match request {
                AnalyzeAsyncRequest::LoadProviders { target, build_config } => {
                    let providers = context.load_providers(&target, build_config).await?;
                    Ok(AnalyzeEvent::Providers { providers })
                }
                AnalyzeAsyncRequest::LoadGlobalProvider { provider, build_config } => {
                    let provider = context.load_global_provider(&provider, build_config).await?;
                    match provider {
                        Some(provider) => Ok(AnalyzeEvent::Provider { provider }),
                        None => Ok(AnalyzeEvent::None),
                    }
                }
                AnalyzeAsyncRequest::ActionOutput {
                    action,
                    partial_actions,
                } => {
                    let current_target_label = this.lock().await.target_label.clone();
                    let partial_actions = partial_actions
                        .into_iter()
                        .map(|action| (current_target_label.join_action(&action.id).unwrap(), action))
                        .collect();

                    let output = context.run_action(action, partial_actions).await?;
                    Ok(AnalyzeEvent::ActionOutput(output))
                }
                AnalyzeAsyncRequest::LabelOpen { label } => {
                    let Some(reference) = context.load_file_label(&label).await? else {
                        return Ok(AnalyzeEvent::None)
                    };
                    let subpath = reference
                        .subpath
                        .as_ref()
                        .map(|x| x.as_ref())
                        .with_context(|| format!("attempted to open depmap root as file {}", label))?;
                    let depmap = context.get_filetree(&reference.hash).await?;
                    let Some(file_entry) = depmap.get(subpath)? else {
                        return Ok(AnalyzeEvent::None)
                    };
                    let handle = match file_entry {
                        FileEntryRef::Regular {
                            content_hash,
                            executable,
                        } => context.open_cache_file(content_hash, executable).await?,
                        FileEntryRef::Symlink(_) => todo!(),
                        FileEntryRef::Directory => bail!("attempt to open directory at {}", label),
                    };
                    let fd = this.lock().await.runtime.inject_fd(handle)?;
                    Ok(AnalyzeEvent::FileHandle { fileno: fd.into() })
                }
                AnalyzeAsyncRequest::ConcreteDepmapFileOpen { depmap, filename } => {
                    let filename = filename.normalize_require_descending().context("path escapes root")?;
                    let depmap = context.get_filetree(&depmap).await?;
                    let Some(file_entry) = depmap.get(filename.as_ref())? else {
                        return Ok(AnalyzeEvent::None)
                    };
                    let handle = match file_entry {
                        FileEntryRef::Regular {
                            content_hash,
                            executable,
                        } => context.open_cache_file(content_hash, executable).await?,
                        FileEntryRef::Symlink(_) => todo!(),
                        FileEntryRef::Directory => bail!("attempt to open directory at {:?} within depmap", filename),
                    };
                    let fd = this.lock().await.runtime.inject_fd(handle)?;
                    Ok(AnalyzeEvent::FileHandle { fileno: fd.into() })
                }
                AnalyzeAsyncRequest::ConcreteDepmapDirectoryList { depmap, filename } => {
                    let filename = filename.normalize_require_descending().context("path escapes root")?;
                    let depmap = context.get_filetree(&depmap).await?;
                    let mut filenames = Vec::new();
                    for entry in depmap.iter() {
                        let (k, v) = entry?;
                        if let Some(subpath) =  k.strip_prefix(&filename) && !subpath.as_str().is_empty() && !subpath.as_str().contains("/") {
                            filenames.push(subpath.to_owned().into_inner());
                        }
                    }
                    Ok(AnalyzeEvent::FilenameList { filenames })
                }
                AnalyzeAsyncRequest::ContentRefOpen { hash } => {
                    let handle = context.open_cache_file(hash.as_ref(), false).await?;
                    let fd = this.lock().await.runtime.inject_fd(handle)?;
                    Ok(AnalyzeEvent::FileHandle { fileno: fd.into() })
                }
                AnalyzeAsyncRequest::FileExists { label } => {
                    let exists = context.labeled_file_exists(&label).await?;
                    Ok(AnalyzeEvent::Boolean { value: exists })
                }
                AnalyzeAsyncRequest::IsFile { label } => {
                    let is_file = context.labeled_file_is_file(&label).await?;
                    Ok(AnalyzeEvent::Boolean { value: is_file })
                }
                AnalyzeAsyncRequest::TargetExists { label } => {
                    let exists = context.labeled_target_exists(&label).await?;
                    Ok(AnalyzeEvent::Boolean { value: exists })
                }
            }
        }
        .instrument(span)
    }
}

#[derive(Clone)]
struct LoaderApi(Arc<_LoaderApi>);

struct _LoaderApi {
    filesystems: Vec<(String, Arc<dyn Handle>)>,
}

impl Api for LoaderApi {}

impl ApiDispatch for LoaderApi {
    fn filesystems(&self) -> &[(String, Arc<dyn Handle>)] {
        &self.0.filesystems
    }
}
