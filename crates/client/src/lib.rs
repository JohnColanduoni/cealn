cfg_if::cfg_if! {
    if #[cfg(target_os = "linux")] {
        #[path = "platform/linux.rs"]
        mod platform;
    } else if #[cfg(target_os = "macos")] {
        #[path = "platform/macos.rs"]
        mod platform;
    } else {
        compile_error!("unsupported platform");
    }
}

pub use cealn_data::action::StructuredMessageLevel;
pub use cealn_protocol::{
    event::{BuildEvent, BuildEventData, BuildEventSource, InternalError},
    query::{StdioLine, StdioStreamType},
    workspace_builder::{grpc, BuildRequest, RunRequest},
};

use std::{
    collections::BTreeMap,
    convert::TryFrom,
    env,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    str::FromStr,
    time::{Duration, Instant},
};

use anyhow::{anyhow, bail, Context, Result};
use futures::{prelude::*, Stream};
use tonic::{transport::Channel, Request};
use tracing::{debug, debug_span, error, info, trace};
use url::Url;

use cealn_core::{trace_call_result, tracing::error_value};
use cealn_data::LabelBuf;
use cealn_protocol::{
    workspace_builder::{grpc::client::WorkspaceBuilderClient, ServerStatus},
    ServerContext,
};

use crate::platform::{Pid, Process};

pub struct Client {
    options: ClientOptions,
    grpc_client: WorkspaceBuilderClient<Channel>,
}

#[derive(Debug)]
pub enum OutOfDateReason {
    StatusParseFailure,
    ExecutableOutOfDate,
    EnvironmentChanged {
        changed_variables: Vec<OsString>,
        new_variables: Vec<OsString>,
        removed_variables: Vec<OsString>,
    },
}

#[derive(Clone, Debug)]
pub struct ClientOptions {
    /// The default package to use with labels that are package or workspace relative (e.g. `:mytarget` or `//mypackage:mytarget`)
    ///
    /// If not specified, this is the same as the root workspace. Note that the root workspace determines the root
    /// build context, so this option is crucial if you want to implicitly run commands in another workspace without
    /// starting a completely separate build.
    pub default_package: Option<LabelBuf>,

    pub jobs: Option<usize>,
}

impl Client {
    #[tracing::instrument(level = "info", err)]
    pub async fn launch_or_connect(workspace_root: &Path, build_root: &Path, options: ClientOptions) -> Result<Self> {
        let server_context = ServerContext::new(workspace_root, build_root)?;

        let environment_variables = Self::prepare_server_environment();

        for _ in 0usize..10usize {
            let server_process = match Self::check_server_pid_file(&server_context).await? {
                Some(process) => process,
                None => Self::launch(&server_context, &environment_variables, &options).await?,
            };

            let api_url = Self::read_api_url(&server_context, &server_process).await?;

            let mut grpc_client = WorkspaceBuilderClient::connect(api_url.to_string()).await?;

            let server_out_of_date = Self::check_server_up_to_date(&mut grpc_client, &environment_variables).await?;

            if let Some(reason) = server_out_of_date {
                // FIXME: don't print this here
                eprintln!("restarting server: {:?}", reason);
                info!(code = "stopping_out_of_date_server");
                // Out of date, kill the server and then try to restart it
                match grpc_client.stop(grpc::StopRequest {}).await {
                    Ok(_) => {}
                    Err(err) => {
                        error!(code = "stop_request_failed", error = error_value(&err));
                        // This is okay-ish, still try to restart server
                    }
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }

            return Ok(Client { options, grpc_client });
        }

        bail!("server restarted too many times during startup");
    }

    #[tracing::instrument(level = "info", err)]
    async fn launch(
        server_context: &ServerContext,
        environment_variables: &BTreeMap<OsString, OsString>,
        options: &ClientOptions,
    ) -> Result<Process> {
        // Delete any pre-existing API files to avoid us being confused
        // TODO: we should probably do some advisory locking here to deal with launch races, or use dev:inode to ignore
        // the file if it's identical to the one in place before launch.
        match fs::remove_file(server_context.api_url_file_path()) {
            Ok(()) => {}
            // File didn't exist, that's fine
            Err(ref err) if err.kind() == io::ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }

        let server_executable = Self::server_executable()?;

        info!(code = "launching_server", ?server_executable);

        // TODO: allow calling the client from processes other than the main binary
        let mut server_command = Command::new(&server_executable);

        server_command.arg("server").arg("--detach");

        if let Some(jobs) = options.jobs {
            server_command.arg("--jobs").arg(jobs.to_string());
        }

        server_command
            .arg(&server_context.workspace_root)
            .arg(&server_context.build_root);

        server_command.env_clear();
        for (k, v) in environment_variables.iter() {
            server_command.env(k, v);
        }

        let pid = {
            use std::io::Read;

            server_command
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .stdin(Stdio::null());

            // The server forks (Unix) or relaunches (Windows) so we should wait so we cleanup the intermediate process
            // It will emit the "real" server process's PID on stdout
            let mut child = server_command.spawn()?;
            let exit_status = child.wait()?;
            debug!(code = "intermediate_exit_status", ?exit_status);
            if !exit_status.success() {
                bail!("server exited during startup");
            }
            let stdout = {
                let span = debug_span!("read_forked_pid");
                let _guard = span.enter();
                let mut stdout = String::new();
                child.stdout.take().unwrap().read_to_string(&mut stdout)?;
                stdout
            };
            Pid::from_str(&stdout).with_context(|| format!("server exited during startup"))?
        };

        debug!(code = "launched_server", pid);

        Self::check_server_pid(pid)
            .await?
            .ok_or_else(|| anyhow!("server exited during startup"))
    }

    #[tracing::instrument(level = "debug", err)]
    async fn check_server_pid_file(server_context: &ServerContext) -> Result<Option<Process>> {
        let pid = match fs::read_to_string(server_context.pid_file_path()) {
            Ok(pid_string) => match pid_string.parse::<Pid>() {
                Ok(pid) => {
                    debug!(code = "read_pid_file", pid);
                    pid
                }
                Err(err) => {
                    error!(
                        code = "invalid_pid_in_pid_file",
                        error = error_value(&err),
                        "invalid PID in pidfile: {}",
                        err
                    );
                    return Ok(None);
                }
            },
            Err(ref err) if err.kind() == io::ErrorKind::NotFound => {
                debug!(code = "no_pid_file");
                return Ok(None);
            }
            Err(err) => return Err(err.into()),
        };

        Self::check_server_pid(pid).await
    }

    #[tracing::instrument(level = "trace", err)]
    async fn check_server_pid(pid: Pid) -> Result<Option<Process>> {
        Ok(Process::get(pid)?)
    }

    #[tracing::instrument(level = "debug", err, skip(grpc_client))]
    async fn check_server_up_to_date(
        grpc_client: &mut WorkspaceBuilderClient<Channel>,
        environment_variables: &BTreeMap<OsString, OsString>,
    ) -> Result<Option<OutOfDateReason>> {
        let status = grpc_client.status(grpc::ServerStatusRequest {}).await?.into_inner();
        let status = match ServerStatus::try_from(status) {
            Ok(status) => status,
            Err(err) => {
                // We assume any error in parsing server status is due to the server being out of date and kill it
                error!(code = "invalid_status_response", error = error_value(&err));
                return Ok(Some(OutOfDateReason::StatusParseFailure));
            }
        };

        debug!(code = "status_response", ?status);

        // Check if the server binary has changed
        // FIXME: this is kind of vague, should handle things like path changes too and maybe use hash
        let server_executable = Self::server_executable()?;
        let server_executable_metadata = trace_call_result!(fs::metadata(&server_executable))?;
        let server_executable_mtime = server_executable_metadata.modified()?;
        if status.server_executable_mtime < server_executable_mtime {
            debug!(
                info = "server_executable_out_of_date",
                expected_mtime = ?server_executable_mtime,
                actual_mtime = ?status.server_executable_mtime);
            return Ok(Some(OutOfDateReason::ExecutableOutOfDate));
        }

        if status.launch_environment_variables != *environment_variables {
            let mut changed_variables = Vec::new();
            let mut new_variables = Vec::new();
            let mut removed_variables = Vec::new();
            for (k, v) in environment_variables {
                match status.launch_environment_variables.get(k) {
                    Some(other_v) if other_v != v => {
                        changed_variables.push(k.to_owned());
                    }
                    None => {
                        new_variables.push(k.to_owned());
                    }
                    _ => {}
                }
            }
            for k in status.launch_environment_variables.keys() {
                if !environment_variables.contains_key(k) {
                    removed_variables.push(k.to_owned());
                }
            }
            debug!(
                info = "server_environment_changed",
                ?changed_variables,
                ?new_variables,
                ?removed_variables,
            );
            return Ok(Some(OutOfDateReason::EnvironmentChanged {
                changed_variables,
                new_variables,
                removed_variables,
            }));
        }

        Ok(None)
    }

    #[tracing::instrument(level = "debug", err)]
    fn server_executable() -> Result<PathBuf> {
        if let Some(exe_path) = env::var_os("CEALN_DRIVER") {
            return Ok(PathBuf::from(exe_path));
        }

        let current_exe = env::current_exe()?;
        if current_exe.file_stem() == Some(OsStr::new("cealn")) {
            return Ok(current_exe);
        }
        let cealn_exe = which::which("cealn")?;
        Ok(cealn_exe)
    }

    // Filter environment variables available to the server
    //
    // We use this to both limit the effects of environment variables on the build process (though actions have their
    // own much more agressive environment filtering), and so we can restart the server when these change to ensure a
    // consistent experience across invocations. Otherwise the behavior of the build will vary depending on whether
    // the server was previously run with a client with different environment variables set.
    //
    // Note that this does not affect the underlying hermiticity of the build process itself: client environment
    // variables are not allowed to influence actions directly.
    pub fn prepare_server_environment() -> BTreeMap<OsString, OsString> {
        let mut output = BTreeMap::new();

        let mut env_vars_passthrough: Vec<&'static str> = vec![
            // Rust debug logging configuration
            "CEALN_LOG",
            // Rust backtrace symbolication options
            "CEALN_BACKTRACE",
            // Wasmtime backtrace symbolication options
            "WASMTIME_BACKTRACE_DETAILS",
            // Docker registry client config
            "DOCKER_CONFIG",
            // Opentelemetry collector
            "OTEL_EXPORTER_OTLP_ENDPOINT",
            "OTEL_EXPORTER_OTLP_PROTOCOL",
        ];

        if let Some(value) = env::var_os("CEALN_BACKTRACE") {
            output.insert(OsString::from("RUST_BACKTRACE"), value);
        }

        // Platform specific
        cfg_if::cfg_if! {
            if #[cfg(unix)] {
                // Use an unoffensive default path
                output.insert(OsString::from("PATH"), OsString::from("/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"));
                env_vars_passthrough.extend(&[
                    // Logged in user info
                    "USER", "HOME", "LOGNAME",
                    // Chosen temporary directory
                    "TMPDIR",
                    // Encoding/collation info
                    "LANG", "LC_COLLATE", "LC_CTYPE", "LC_MONETARY", "LC_NUMERIC", "LC_TIME", "LC_MESSAGES", "LC_ALL",
                    // May be needed for git actions
                    "SSH_AUTH_SOCK"
                ]);

                cfg_if::cfg_if! {
                    if #[cfg(target_vendor = "apple")] {
                        env_vars_passthrough.extend(&[
                            // Collation
                            "__CF_USER_TEXT_ENCODING",
                        ]);
                    }
                }
            } else if #[cfg(target_os = "windows")] {
                env_vars_passthrough.extend(&[
                    // Core environment variables that Win32 malfunctions without
                    "SYSTEMROOT", "SYSTEMDRIVE", "WINDIR", "PATHEXT", "OS",
                    // Logged in user info
                    "USERNAME", "USERPROFILE",
                    // Chosen temporary directory
                    "TEMP", "TMP",
                    // May be needed for git actions
                    "GIT_ASKPASS", "GIT_SSH",
                ]);
            } else {
                compile_error!("not implemented for platform");
            }
        }

        for var_name in env_vars_passthrough.iter() {
            if let Some(value) = env::var_os(var_name) {
                output.insert(OsString::from(var_name), value);
            }
        }

        debug!(code = "prepared_server_environment", vars = ?output);

        output
    }

    #[tracing::instrument(level = "debug", err)]
    async fn read_api_url(server_context: &ServerContext, process: &Process) -> Result<Url> {
        let start = Instant::now();

        debug!(code = "reading_api_url", api_url = ?server_context.api_url_file_path());

        loop {
            match fs::read_to_string(server_context.api_url_file_path()) {
                Ok(url_string) => {
                    let url = Url::parse(&url_string).with_context(|| format!("server provided invalid API URL"))?;
                    debug!(code = "api_url_fetched", %url);
                    return Ok(url);
                }
                Err(ref err) if err.kind() == io::ErrorKind::NotFound => {
                    if Instant::now() - start < API_URL_FILE_TIMEOUT {
                        trace!(code = "api_url_fetch_backoff");
                        tokio::time::sleep(API_URL_POLL_INTERVAL).await;
                    } else {
                        error!(code = "api_url_fetch_timeout");
                        bail!("server did not publish it's API URL in time");
                    }
                }
                Err(err) => return Err(err.into()),
            }

            if !process.running()? {
                bail!("server exited during startup");
            }
        }
    }

    #[tracing::instrument(level = "info", err, skip(self))]
    pub async fn build(&mut self, mut request: BuildRequest) -> Result<impl Stream<Item = Result<BuildEvent>>> {
        if request.default_package.is_none() {
            request.default_package = self.options.default_package.clone();
        }

        let event_stream = self.grpc_client.build(Request::new(request.into())).await?.into_inner();

        Ok(event_stream.err_into::<anyhow::Error>().and_then(|event| async move {
            let event = BuildEvent::try_from(event)?;
            // TODO: expand event fields
            trace!(code = "build_event", ?event);
            Ok(event)
        }))
    }

    #[tracing::instrument(level = "info", err, skip(self))]
    pub async fn run(&mut self, mut request: RunRequest) -> Result<impl Stream<Item = Result<BuildEvent>>> {
        if request.default_package.is_none() {
            request.default_package = self.options.default_package.clone();
        }

        let stream = stream::once(async move { grpc::RunRequest::from(request) });

        let event_stream = self.grpc_client.run(Request::new(stream)).await?.into_inner();

        Ok(event_stream.err_into::<anyhow::Error>().and_then(|event| async move {
            let event = BuildEvent::try_from(event)?;
            // TODO: expand event fields
            trace!(code = "build_event", ?event);
            Ok(event)
        }))
    }
}

/// The maximum duration to wait before timing out while waiting for the API url file to appear
const API_URL_FILE_TIMEOUT: Duration = Duration::from_secs(30);

/// The frequency to check for the API url file's existence
const API_URL_POLL_INTERVAL: Duration = Duration::from_millis(10);

impl Default for ClientOptions {
    fn default() -> Self {
        ClientOptions {
            default_package: None,
            jobs: None,
        }
    }
}
