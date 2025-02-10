use std::{
    backtrace::Backtrace,
    collections::BTreeMap,
    env,
    ffi::OsString,
    fs,
    fs::{File, OpenOptions},
    path::{Path, PathBuf},
    process::{self, Command},
    time::{Duration, Instant, SystemTime},
};

use anyhow::{bail, Result};
use cealn_cache::HotDiskCache;
use cealn_event::EventContext;
use cealn_runtime_data::rule::AnalyzeAsyncRequest;
use cealn_source::SourceMonitor;
use chrono::{DateTime, Utc};
use fs3::FileExt;
use futures::{
    channel::mpsc,
    prelude::*,
    stream::{FuturesOrdered, FuturesUnordered},
};
use serde::Serialize;
use tracing::debug;

use cealn_core::trace_call_result;
use cealn_data::label::JoinError;
use cealn_protocol::{
    event::{BuildEvent, BuildEventData, InternalError},
    query::{AnalysisQuery, AnalysisQueryProduct, OutputQuery, OutputQueryProduct},
    workspace_builder::{AnalyzeRequest, BuildRequest},
    ServerContext,
};
use cealn_runtime::interpreter;

use crate::{
    error::QueryCallErrorContext,
    executor::Executor,
    graph::{Graph, QueryError},
};

pub struct WorkspaceBuilder {
    pub(crate) server_context: ServerContext,
    lock_file: Option<File>,

    server_executable_mtime: SystemTime,
    launch_environment_variables: BTreeMap<OsString, OsString>,

    _executor: Executor,
    pub(crate) graph: Graph,
}

pub struct Options {
    pub jobs: Option<usize>,
}

impl WorkspaceBuilder {
    #[tracing::instrument(level = "info", err, skip(options))]
    pub async fn start(workspace_root: &Path, build_root: &Path, options: &Options) -> Result<Self> {
        // Remember executable mtime
        // TODO: maybe switch to using client (device_id, inode_id) and Windows equivalent? Executables are generally
        // replaced instead of modified in place.
        // TODO: this is probably fine since we do it very early in server startup but there is a possible race here
        // since we open using the path. We should see if we can do it with fstat equivalent to handle atomic writes
        // to the executable path that will unlink our copy.
        let executable_path = trace_call_result!(env::current_exe())?;
        let executable_metadata = trace_call_result!(fs::metadata(&executable_path))?;
        let server_executable_mtime = executable_metadata.modified()?;
        debug!(
            code = "recorded_server_executable_mtime",
            mtime = ?server_executable_mtime
        );

        // Remember launch environment variables
        let launch_environment_variables: BTreeMap<OsString, OsString> = env::vars_os().collect();

        let server_context = ServerContext::new(workspace_root, build_root)?;

        let lock_file = Self::acquire_lock(&server_context)?;

        trace_call_result!(fs::write(server_context.pid_file_path(), process::id().to_string()))?;
        trace_call_result!(fs::create_dir_all(server_private_dir_path(&server_context)))?;

        let executor = Executor::new(crate::executor::Options {
            thread_pool_concurrency: None,
            process_concurrency: options.jobs,
        })?;

        let depset_registry = cealn_depset::Registry::new();
        let primary_cache = HotDiskCache::open(&primary_cache_path(&server_context), depset_registry.clone())?;
        let source_view = SourceMonitor::new(server_context.canonical_workspace_root.clone()).await?;

        // Setup interpreter WASM cache
        let cache_config_path = wasmtime_cache_config_file_path(&server_context);
        let cache_path = wasmtime_cache_storage_path(&server_context);
        let cache_config = WasmtimeCacheConfigFile {
            cache: WasmtimeCacheConfig {
                enabled: true,
                // FIXME: ensure this correctly handles non-compliant paths
                directory: cache_path.display().to_string(),
            },
        };
        let serialized_cache_config = toml::ser::to_vec(&cache_config).unwrap();
        trace_call_result!(fs::write(&cache_config_path, serialized_cache_config))?;

        let temporary_directory = temporary_file_path(&server_context);
        cleanup_temporary_directory(&temporary_directory)?;
        fs::create_dir_all(&temporary_directory)?;

        let graph = Graph::new(
            source_view,
            executor.clone(),
            primary_cache,
            interpreter::Options {
                cache_config_file: Some(cache_config_path),
            },
            temporary_directory,
            depset_registry,
        )
        .await?;

        let builder = WorkspaceBuilder {
            server_context,
            lock_file: Some(lock_file),
            server_executable_mtime,
            launch_environment_variables,

            _executor: executor,
            graph,
        };

        Ok(builder)
    }

    #[tracing::instrument(level = "info", err, skip(self))]
    pub async fn stop(mut self) -> Result<()> {
        debug!(code = "releasing_lock");
        std::mem::drop(self.lock_file.take());

        debug!(code = "removing_pid_file");
        trace_call_result!(fs::remove_file(self.server_context.pid_file_path()))?;

        Ok(())
    }

    pub fn server_context(&self) -> &ServerContext {
        &self.server_context
    }

    pub fn server_executable_mtime(&self) -> &SystemTime {
        &self.server_executable_mtime
    }

    pub fn launch_environment_variables(&self) -> &BTreeMap<OsString, OsString> {
        &self.launch_environment_variables
    }

    #[tracing::instrument(level = "debug", err)]
    fn acquire_lock(server_context: &ServerContext) -> Result<File> {
        // Acquire lock file
        // TODO: I believe on Windows it will fail during opening if the file is locked, instead of during locking
        let lock_file = OpenOptions::new()
            .write(true)
            .create(true)
            .open(server_context.lock_file_path())?;
        if let Err(err) = lock_file.try_lock_exclusive() {
            if err.raw_os_error() == fs3::lock_contended_error().raw_os_error() {
                bail!("already running");
            } else {
                return Err(err.into());
            }
        }

        Ok(lock_file)
    }

    #[tracing::instrument("Workspace::build", level = "info", err, parent = None, skip(self, events))]
    pub async fn build(&self, request: BuildRequest, mut events: EventContext) -> Result<Vec<OutputQueryProduct>> {
        if let Some(default_package) = &request.default_package {
            if default_package.is_package_relative() {
                bail!("default package must not be a package-relative path");
            }
        }

        let concrete_targets: Vec<_> = request
            .targets
            .iter()
            .map(|target_label| {
                if let Some(default_package) = &request.default_package {
                    match default_package.join(target_label) {
                        Ok(label) => Ok(label),
                        // This can't happen because the default package must be a package, not a target
                        Err(JoinError::MultiplePackageSeparators) => unreachable!(),
                    }
                } else if target_label.is_package_relative() {
                    bail!("target is package relative, but no default package was provided");
                } else {
                    return Ok(target_label.clone());
                }
            })
            .collect::<Result<Vec<_>, _>>()?;

        // Use consistent request time so common dependencies of queries don't build multiple times
        let request_time = Instant::now();
        let mut any_change_guard = self.graph.0.source_view.will_observe_any_change();

        let mut output_queries = FuturesOrdered::new();

        if request.watch {
            events.send(BuildEventData::WatchRun);
        }

        for target_label in concrete_targets.iter() {
            output_queries.push(self.graph.query_output(
                OutputQuery {
                    target_label: target_label.to_owned(),
                    build_config: request.build_config.clone(),
                },
                request_time,
                events.fork(),
            ));
        }

        let mut output_products = Vec::new();
        let mut errors = Vec::new();
        while let Some(result) = output_queries.next().await {
            debug!(?result, "query completed");
            match result.output() {
                Ok(product) => {
                    output_products.push(product.clone());
                }
                Err(err) => {
                    errors.push(err.clone());
                    events.send(BuildEventData::InternalError(prepare_internal_error(err)));
                    if !request.keep_going && !request.watch {
                        break;
                    }
                }
            }
        }

        if !request.watch {
            // FIXME: aggregate
            if let Some(error) = errors.pop() {
                Err(error.into())
            } else {
                Ok(output_products)
            }
        } else {
            loop {
                events.send(BuildEventData::WatchIdle);
                self.graph.0.source_view.wait_for_any_change(any_change_guard).await?;
                events.send(BuildEventData::WatchRun);

                any_change_guard = self.graph.0.source_view.will_observe_any_change();

                // Debounce updates
                for _ in 0..50 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    if self
                        .graph
                        .0
                        .source_view
                        .any_change_guard_check_dirty(&mut any_change_guard)?
                    {
                        continue;
                    } else {
                        break;
                    }
                }

                let request_time = Instant::now();
                for target_label in &concrete_targets {
                    output_queries.push(self.graph.query_output(
                        OutputQuery {
                            target_label: target_label.to_owned(),
                            build_config: request.build_config.clone(),
                        },
                        request_time,
                        events.fork(),
                    ));
                }

                let mut output_products = Vec::new();
                let mut errors = Vec::new();
                while let Some(result) = output_queries.next().await {
                    debug!(?result, "query completed");
                    match result.output() {
                        Ok(product) => {
                            output_products.push(product.clone());
                        }
                        Err(err) => {
                            errors.push(err.clone());
                            events.send(BuildEventData::InternalError(prepare_internal_error(err)));
                            if !request.keep_going && !request.watch {
                                break;
                            }
                        }
                    }
                }

                // Wait to poll again if it was too quick
                tokio::time::sleep_until((request_time + Duration::from_millis(100)).into()).await;
            }
        }
    }

    pub async fn analyze(&self, request: AnalyzeRequest, mut events: EventContext) -> Result<AnalysisQueryProduct> {
        if let Some(default_package) = &request.default_package {
            if default_package.is_package_relative() {
                bail!("default package must not be a package-relative path");
            }
        }

        let concrete_target = if let Some(default_package) = &request.default_package {
            match default_package.join(&request.target) {
                Ok(label) => label,
                // This can't happen because the default package must be a package, not a target
                Err(JoinError::MultiplePackageSeparators) => unreachable!(),
            }
        } else if request.target.is_package_relative() {
            bail!("target is package relative, but no default package was provided");
        } else {
            request.target.clone()
        };

        // Use consistent request time so common dependencies of queries don't build multiple times
        let request_time = Instant::now();

        let result = self
            .graph
            .query_analysis(
                AnalysisQuery {
                    target_label: request.target,
                    build_config: request.build_config.clone(),
                },
                request_time,
                events.fork(),
            )
            .await;

        match result.output() {
            Ok(product) => Ok(product.clone()),
            Err(err) => {
                events.send(BuildEventData::InternalError(prepare_internal_error(err)));
                Err(err.clone().into())
            }
        }
    }
}

fn primary_cache_path(context: &ServerContext) -> PathBuf {
    context.build_root.join("cache")
}

fn temporary_file_path(context: &ServerContext) -> PathBuf {
    context.build_root.join("tmp")
}

fn server_private_dir_path(context: &ServerContext) -> PathBuf {
    context.build_root.join("server")
}

fn wasmtime_cache_config_file_path(context: &ServerContext) -> PathBuf {
    server_private_dir_path(context).join("wasmtime_cache.toml")
}

fn wasmtime_cache_storage_path(context: &ServerContext) -> PathBuf {
    server_private_dir_path(context).join("wasmtime_cache")
}

fn format_backtrace(backtrace: &dyn std::fmt::Display) -> Option<Vec<String>> {
    // TODO: check status
    Some(backtrace.to_string().split('\n').map(|x| x.to_owned()).collect())
}

#[derive(Serialize, Debug)]
struct WasmtimeCacheConfigFile {
    cache: WasmtimeCacheConfig,
}

#[derive(Serialize, Debug)]
struct WasmtimeCacheConfig {
    enabled: bool,
    directory: String,
}

fn prepare_internal_error(err: &QueryError) -> InternalError {
    let outer_anyhow = err.inner();
    let mut prev_cause = None;
    for inner_cause in outer_anyhow.chain() {
        if let Some(query_error) = inner_cause.downcast_ref::<QueryError>() {
            prev_cause = Some(Box::new(InternalError {
                message: query_error.to_string(),
                backtrace: format_backtrace(query_error.inner().backtrace()).unwrap_or_default(),
                cause: prev_cause,
                nested_query: true,
            }))
        } else {
            prev_cause = Some(Box::new(InternalError {
                message: inner_cause.to_string(),
                backtrace: inner_cause
                    .request_ref::<Backtrace>()
                    .and_then(|x| format_backtrace(x))
                    .unwrap_or_default(),
                cause: prev_cause,
                nested_query: false,
            }))
        }
    }
    InternalError {
        message: outer_anyhow.to_string(),
        backtrace: format_backtrace(outer_anyhow.backtrace()).unwrap_or_default(),
        nested_query: true,
        cause: prev_cause,
    }
}

fn cleanup_temporary_directory(path: &Path) -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let mounts = proc_mounts::MountList::new()?;
        for mount in mounts.source_starts_with(Path::new("cealn-depmap")) {
            if !mount.dest.strip_prefix(path).is_ok() {
                continue;
            }
            let status = Command::new("fusermount3").arg("-u").arg(&mount.dest).status()?;
            if !status.success() {
                bail!("unmount via fusermount3 failed on {:?}", mount.dest);
            }
        }

        // We may have dead fuse mounts in the temporary directory, clean these up first
    }
    // Allow failing to remove some files
    let _ = std::fs::remove_dir_all(path);
    Ok(())
}
