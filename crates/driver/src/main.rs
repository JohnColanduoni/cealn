use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    fmt::Write,
    fs,
    io::{self, Write as IoWrite},
    mem,
    path::{Path, PathBuf},
    process,
};

use anyhow::{bail, Result};
use clap::{CommandFactory, Parser, Subcommand};
use convert_case::{Case, Casing};
use futures::{pin_mut, prelude::*};
use mimalloc::MiMalloc;
use serde::ser::{SerializeMap, SerializeSeq};
use target_lexicon::{Architecture, Triple};
use tracing::{debug, error};

use cealn_cli_support::{console, create_client, host_build_config, logging, triple_build_config, ClientOpts};
use cealn_client::{BuildEventData, BuildRequest, Client, ClientOptions, RunRequest, StructuredMessageLevel};
use cealn_core::{
    files::{workspace_file_exists_in, WellKnownFileError},
    trace_call_result,
    tracing::error_value,
};
use cealn_data::{reference::Reference, rule::BuildConfig, Label, LabelBuf};
use cealn_server::{
    api::Server,
    builder::{self, WorkspaceBuilder},
};
use io::LineWriter;

use crate::console::{Console, ConsoleOptions};

#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

#[derive(Parser)]
#[clap(name = "cealn", version = "0.0.0")]
struct Opts {
    /// Enable debug level logging
    #[clap(long = "debug-self")]
    debug: bool,

    #[clap(subcommand)]
    sub_command: SubCommand,
}

#[derive(Subcommand)]
enum SubCommand {
    /// Build one or more targets
    Build(BuildOpts),
    /// Run an executable built by cealn on the current directory
    Run(RunOpts),
    /// Runs a build server instance
    ///
    /// You generally won't need to do this explicitly; Other commands will automatically connect to or start a
    /// server as appropriate.
    Server(ServerOpts),
    #[clap(hide = true)]
    ShellAutocomplete(ShellAutocompleteOpts),
}

#[derive(Parser, Debug)]
pub struct BuildOpts {
    #[clap(flatten)]
    client: ClientOpts,

    #[clap(long, short)]
    quiet: bool,

    /// If errors are encountered during the build, continue running other tasks
    #[clap(long)]
    keep_going: bool,

    #[clap(long)]
    watch: bool,

    /// Print the events from actions that were loaded from the cache
    #[clap(long)]
    print_cached_output: bool,

    #[clap(long, default_value = "warn")]
    structured_message_max_level: StructuredMessageLevel,

    #[clap(long)]
    structured_message_stdout: bool,

    #[clap(long)]
    opt: bool,

    #[clap(long = "target")]
    target_triple: Option<Triple>,

    /// A list of target labels to build
    ///
    /// Relative labels (e.g. `//mypackage:mytarget` or `:mytarget`) will be interpreted according to the default
    /// package.
    #[clap(name = "TARGET", required = true, num_args(1..))]
    targets: Vec<LabelBuf>,
}

#[derive(Parser, Debug)]
#[clap(disable_help_flag = true)]
pub struct RunOpts {
    #[clap(flatten)]
    client: ClientOpts,

    #[clap(name = "TARGET", required = true)]
    target: LabelBuf,

    #[clap(name = "EXECUTABLE", required = true)]
    executable_name: String,

    #[clap(long)]
    override_entrypoint: Option<String>,

    #[clap(long)]
    debug: bool,

    #[clap(trailing_var_arg = true, allow_hyphen_values = true)]
    cmd_args: Vec<OsString>,
}

#[derive(Parser, Debug)]
pub struct ServerOpts {
    #[clap(name = "WORKSPACE_ROOT")]
    workspace_root: PathBuf,

    #[clap(name = "BUILD_ROOT")]
    build_root: PathBuf,

    /// Detaches the server from the current session/terminal and run it in the background
    #[clap(long)]
    detach: bool,

    #[clap(long)]
    jobs: Option<usize>,
}

#[derive(Parser, Debug)]
pub struct ShellAutocompleteOpts {
    shell: clap_complete::Shell,
}

fn main() {
    let opts = Opts::parse();

    // If we're going to detach, we need to fork very early on
    let mut is_server = false;
    match &opts.sub_command {
        SubCommand::Server(server_opts) => {
            is_server = true;
            if server_opts.detach {
                if !pre_detach() {
                    process::exit(1);
                }
            }
        }
        _ => {}
    };

    let evloop = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    // Initialize logging
    let log_guard = {
        let _evloop_guard = evloop.enter();
        logging::init(opts.debug, is_server)
    };
    if opts.debug {
        if !env::var_os("CEALN_BACKTRACE").is_some() {
            env::set_var("CEALN_BACKTRACE", "1");
        }
    }

    let retval = match opts.sub_command {
        SubCommand::Build(build_opts) => evloop.block_on(build(&build_opts)),
        SubCommand::Run(run_opts) => evloop.block_on(run(&run_opts)),
        SubCommand::Server(server_opts) => evloop.block_on(server(&server_opts)),
        SubCommand::ShellAutocomplete(autocomplete_opts) => evloop.block_on(autocomplete(autocomplete_opts)),
    };

    evloop.block_on(log_guard.flush());
    process::exit(retval);
}

#[tracing::instrument("driver::build", level = "info")]
async fn build(build_opts: &BuildOpts) -> i32 {
    let mut console = Console::new(ConsoleOptions {
        tty: build_opts.client.should_use_terminal(),
        print_cached_output: build_opts.print_cached_output,
        max_level: Some(build_opts.structured_message_max_level),
    });

    let mut client = match create_client(&build_opts.client).await {
        Ok(x) => x,
        Err(err) => {
            // FIXME: better status messaging
            eprintln!("error when attempting to set up client: {}", err);
            return 1;
        }
    };

    let build_config = if let Some(triple) = &build_opts.target_triple {
        triple_build_config(&triple, build_opts.opt)
    } else {
        host_build_config(build_opts.opt)
    };

    let stream = match client
        .build(BuildRequest {
            targets: build_opts.targets.clone(),
            // Default from client settings is fine
            default_package: None,
            build_config,
            keep_going: build_opts.keep_going,
            watch: build_opts.watch,
        })
        .await
    {
        Ok(x) => x,
        Err(err) => {
            // FIXME: better status messaging
            eprintln!("error when attempting to invoke build on server: {}", err);
            return 1;
        }
    };

    pin_mut!(stream);

    let mut did_error = false;
    let mut stdout = std::io::stdout();
    loop {
        let event = match stream.next().await {
            Some(Ok(event)) => event,
            Some(Err(err)) => {
                // FIXME: better status messaging
                eprintln!("error encountered while streaming build events from server: {}", err);
                return 1;
            }
            None => break,
        };
        debug!(code = "build_event", event = ?event);
        match &event.data {
            BuildEventData::InternalError(_) => {
                did_error = true;
            }
            BuildEventData::WorkspaceFileNotFound { .. } => {
                did_error = true;
            }
            BuildEventData::Message { data, .. } if build_opts.structured_message_stdout => {
                // TODO: don't copy
                let _ = serde_json::to_writer(
                    &mut stdout,
                    &ProstValueJson(&prost_types::Value {
                        kind: Some(prost_types::value::Kind::StructValue(data.clone())),
                    }),
                );
                let _ = writeln!(&mut stdout, "");
            }
            _ => {}
        }
        if !build_opts.quiet {
            console.push_build_event(&event);
        }
    }

    if !did_error {
        0
    } else {
        1
    }
}

#[tracing::instrument("driver::run", level = "info")]
async fn run(run_opts: &RunOpts) -> i32 {
    let mut console = Console::new(ConsoleOptions {
        tty: run_opts.client.should_use_terminal(),
        print_cached_output: false,
        max_level: Some(StructuredMessageLevel::Error),
    });

    let mut client = match create_client(&run_opts.client).await {
        Ok(x) => x,
        Err(err) => {
            // FIXME: better status messaging
            eprintln!("error when attempting to set up client: {}", err);
            return 1;
        }
    };

    // FIXME: debug flag should build with Debug, not Fastbuild
    let build_config = host_build_config(!run_opts.debug);

    let stream = match client
        .run(RunRequest {
            target: run_opts.target.clone(),
            executable_name: run_opts.executable_name.clone(),
            // Default from client settings is fine
            default_package: None,
            build_config,
        })
        .await
    {
        Ok(x) => x,
        Err(err) => {
            // FIXME: better status messaging
            eprintln!("error when attempting to invoke build on server: {}", err);
            return 1;
        }
    };

    pin_mut!(stream);

    let mut did_error = false;
    let mut prepped_parent_pid = None;
    let mut executable_path = None;
    loop {
        let event = match stream.next().await {
            Some(Ok(event)) => event,
            Some(Err(err)) => {
                // FIXME: better status messaging
                eprintln!("error encountered while streaming build events from server: {}", err);
                return 1;
            }
            None => break,
        };
        match &event.data {
            BuildEventData::InternalError(_) => {
                did_error = true;
            }
            BuildEventData::WorkspaceFileNotFound { .. } => {
                did_error = true;
            }
            BuildEventData::ExecutablePrepped {
                executable_path: a_executable_path,
                parent_pid,
            } => {
                executable_path = Some(a_executable_path.clone());
                prepped_parent_pid = Some(*parent_pid);
            }
            _ => {}
        }
        console.push_build_event(&event);
    }

    if did_error {
        return 1;
    }

    let Some(prepped_parent_pid) = prepped_parent_pid else {
        todo!()
    };
    let executable_path = run_opts
        .override_entrypoint
        .clone()
        .unwrap_or_else(|| executable_path.unwrap());

    let workdir = std::env::current_dir().unwrap();

    cealn_action_executable::enter_prepared(
        prepped_parent_pid,
        Path::new(&executable_path),
        &run_opts.cmd_args,
        &workdir,
    )
    .unwrap();

    1
}

#[tracing::instrument(level = "info")]
async fn server(server_opts: &ServerOpts) -> i32 {
    // Mask out environment variables
    let allowed_environment = Client::prepare_server_environment();
    for (k, _) in env::vars_os().collect::<Vec<_>>() {
        if !allowed_environment.contains_key(&k) {
            env::remove_var(k);
        }
    }

    let instance = match WorkspaceBuilder::start(
        &server_opts.workspace_root,
        &server_opts.build_root,
        &builder::Options { jobs: server_opts.jobs },
    )
    .await
    {
        Ok(x) => x,
        Err(err) => {
            // FIXME: better status messaging
            eprintln!("error when attempting to set up build instance: {}", err);
            return 1;
        }
    };

    let server = match Server::bind(instance, ([127, 0, 0, 1], 0).into()).await {
        Ok(x) => x,
        Err(err) => {
            // FIXME: better status messaging
            eprintln!("error when setting up API server: {}", err);
            return 1;
        }
    };

    if server_opts.detach {
        if !detach(&server_opts.build_root) {
            return 1;
        }
    }

    if let Err(err) = server.run().await {
        // FIXME: better status messaging
        eprintln!("error when running build API server: {}", err);
        return 1;
    }

    0
}

async fn autocomplete(autocomplete_opts: ShellAutocompleteOpts) -> i32 {
    clap_complete::generate(
        autocomplete_opts.shell,
        &mut Opts::command(),
        "cealn",
        &mut std::io::stdout(),
    );
    0
}

#[must_use]
#[tracing::instrument(level = "debug")]
fn pre_detach() -> bool {
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            use std::fs::File;
            use std::os::unix::prelude::*;

            use cealn_core::libc_call;

            // Fork and let intermediate process die so we are reparented to init
            // This has to be done early (before we do anything like spin up threads or write to a pid file)
            match unsafe { libc_call!(libc::fork()) } {
                Ok(0) => {
                    // Close stdout so our parent can write our PID and close the pipe (otherwise we'll keep it open)
                    let replacement_file = File::open("/dev/null").unwrap();
                    if let Err(err) = replace_fd(replacement_file.as_raw_fd(), 1) {
                        eprintln!("failed to replace stdout: {}", err);
                        return false;
                    }

                    true
                },
                Ok(n) => {
                    // Tell our parent the PID of the "real" child process
                    print!("{}", n);
                    std::io::stdout().flush().unwrap();
                    unsafe { libc::_exit(0); }
                },
                Err(err) => {
                    eprintln!("failed to fork to daemonize process: {}", err);
                    false

                }
            }
        } else if #[cfg(windows)] {
            use winapi::um::processthreadsapi::{GetCurrentProcess, TerminateProcess};

            // Re-launch ourselves with appropriate creation flags, if necessary
            // In theory we could do this in the launch code, but that would require maintaining assumptions between
            // the two pieces of code, and would make launching from the console with the `--detach` flag inconsistent
            // between platforms.

            if env::var_os(RELAUNCHED_WITH_CORRECT_CREATION_FLAGS_ENV_VAR).is_some() {
                env::remove_var(RELAUNCHED_WITH_CORRECT_CREATION_FLAGS_ENV_VAR);
                return true;
            }

            match detach_windows() {
                Ok(pid) => {
                    // Tell our parent the PID of the "real" child process
                    print!("{}", pid);
                    std::io::stdout().flush().unwrap();
                    // Terminate without running any termination handlers
                    unsafe { TerminateProcess(GetCurrentProcess(), 0) };
                    unreachable!()
                }
                Err(err) => {
                    eprintln!("failed to relaunch to daemonize process: {}", err);
                    false
                }
            }
        } else {
            true
        }
    }
}

struct ProstValueJson<'a>(&'a prost_types::Value);

impl serde::Serialize for ProstValueJson<'_> {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0.kind.as_ref().unwrap() {
            prost_types::value::Kind::NullValue(_) => serializer.serialize_none(),
            prost_types::value::Kind::NumberValue(value) => serializer.serialize_f64(*value),
            prost_types::value::Kind::StringValue(value) => serializer.serialize_str(value),
            prost_types::value::Kind::BoolValue(value) => serializer.serialize_bool(*value),
            prost_types::value::Kind::StructValue(value) => {
                let mut map = serializer.serialize_map(Some(value.fields.len()))?;
                for (k, v) in &value.fields {
                    map.serialize_key(k)?;
                    map.serialize_value(&ProstValueJson(v))?;
                }
                map.end()
            }
            prost_types::value::Kind::ListValue(value) => {
                let mut seq = serializer.serialize_seq(Some(value.values.len()))?;
                for value in &value.values {
                    seq.serialize_element(&ProstValueJson(value))?;
                }
                seq.end()
            }
        }
    }
}

#[cfg(windows)]
const RELAUNCHED_WITH_CORRECT_CREATION_FLAGS_ENV_VAR: &str = "__CEALN_DETACH_RELAUNCHED";

#[cfg(windows)]
fn detach_windows() -> io::Result<u32> {
    use std::{ffi::OsStr, mem, os::windows::prelude::*, ptr};

    use widestring::WideCString;
    use winapi::{
        shared::{
            basetsd::SIZE_T,
            minwindef::{DWORD, MAX_PATH, TRUE},
        },
        um::{
            fileapi::{CreateFileW, OPEN_EXISTING},
            handleapi::INVALID_HANDLE_VALUE,
            libloaderapi::GetModuleFileNameW,
            minwinbase::SECURITY_ATTRIBUTES,
            processenv::{GetCommandLineW, GetStdHandle},
            processthreadsapi::{
                CreateProcessW, InitializeProcThreadAttributeList, UpdateProcThreadAttribute, PROCESS_INFORMATION,
            },
            winbase::{
                CREATE_NEW_PROCESS_GROUP, CREATE_UNICODE_ENVIRONMENT, DETACHED_PROCESS, EXTENDED_STARTUPINFO_PRESENT,
                STARTF_USESTDHANDLES, STARTUPINFOEXW, STD_ERROR_HANDLE,
            },
            winnt::{FILE_ATTRIBUTE_NORMAL, GENERIC_READ, GENERIC_WRITE},
        },
    };

    const PROC_THREAD_ATTRIBUTE_INPUT: DWORD = 0x00020000;
    const PROC_THREAD_ATTRIBUTE_HANDLE_LIST: DWORD = 2 | PROC_THREAD_ATTRIBUTE_INPUT;

    // Obtain original startup information
    let exe_name_cstr = {
        let mut buffer = vec![0u16; MAX_PATH];

        loop {
            let received_len =
                unsafe { GetModuleFileNameW(ptr::null_mut(), buffer.as_mut_ptr(), buffer.len() as DWORD) };
            // If the buffer was big enough, the return value is the length of the string without the null terminator. If it's too large, it's the
            // length of the actual filename *with* the null terminator. So a length >= `buffer.len()` indicates a failure.
            if received_len as usize >= buffer.len() {
                buffer.resize(received_len as usize, 0);
                continue;
            } else if received_len < 1 {
                return Err(io::Error::last_os_error());
            } else {
                // `received_len` doesn't include null terminator
                buffer.truncate(received_len as usize + 1);
                break buffer;
            }
        }
    };

    // Open null file handle to prevent stdin/stdout inheritance

    let null_file = unsafe {
        let mut security_attributes: SECURITY_ATTRIBUTES = mem::zeroed();
        security_attributes.nLength = mem::size_of::<SECURITY_ATTRIBUTES>() as DWORD;
        security_attributes.bInheritHandle = TRUE;

        let handle = CreateFileW(
            WideCString::from_str("nul:").unwrap().as_ptr(),
            GENERIC_READ | GENERIC_WRITE,
            0,
            &mut security_attributes,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        handle
    };

    let mut startup_info: STARTUPINFOEXW = unsafe { mem::zeroed() };
    startup_info.StartupInfo.cb = mem::size_of::<STARTUPINFOEXW>() as DWORD;
    startup_info.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
    // Send input and output to null
    startup_info.StartupInfo.hStdInput = null_file;
    startup_info.StartupInfo.hStdOutput = null_file;
    // Inherit initial stderr, so errors during initialization can still be sent to the console
    startup_info.StartupInfo.hStdError = unsafe { GetStdHandle(STD_ERROR_HANDLE) };

    // Explicitly specify handles to be inherited. This prevents the relaunched process from still having the stdout
    // handle open, causing it to hang when our caller reads the relaunched process pid from it.
    let mut proc_attrib_buffer = unsafe {
        let mut size_out: SIZE_T = 0;
        InitializeProcThreadAttributeList(ptr::null_mut(), 1, 0, &mut size_out);
        let mut proc_attrib_buffer = vec![0u8; size_out];
        if InitializeProcThreadAttributeList(proc_attrib_buffer.as_mut_ptr() as _, 1, 0, &mut size_out) == 0 {
            return Err(io::Error::last_os_error());
        }
        proc_attrib_buffer
    };

    let mut handles_to_inherit = [null_file, startup_info.StartupInfo.hStdError];
    if unsafe {
        UpdateProcThreadAttribute(
            proc_attrib_buffer.as_mut_ptr() as _,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as _,
            handles_to_inherit.as_mut_ptr() as _,
            handles_to_inherit.len() * mem::size_of_val(&handles_to_inherit[0]),
            ptr::null_mut(),
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    startup_info.lpAttributeList = proc_attrib_buffer.as_mut_ptr() as _;

    // Prepare environment
    let mut environment_buffer: Vec<u16> = Vec::new();
    for (k, v) in env::vars_os() {
        environment_buffer.extend(k.encode_wide());
        environment_buffer.push(b'=' as u16);
        environment_buffer.extend(v.encode_wide());
        environment_buffer.push(0);
    }
    // Add marker environment variable
    environment_buffer.extend(OsStr::new(RELAUNCHED_WITH_CORRECT_CREATION_FLAGS_ENV_VAR).encode_wide());
    environment_buffer.push(b'=' as u16);
    environment_buffer.extend(OsStr::new("1").encode_wide());
    environment_buffer.push(0);
    // Terminate buffer
    environment_buffer.push(0);

    let mut process_information: PROCESS_INFORMATION = unsafe { mem::zeroed() };

    if unsafe {
        CreateProcessW(
            exe_name_cstr.as_ptr(),
            GetCommandLineW(),
            ptr::null_mut(),
            ptr::null_mut(),
            TRUE,
            // Detach process from any console, and create a new process group so it doesn't receive CTRL-C/CTRL-BREAK
            // events
            CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT | DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP,
            environment_buffer.as_mut_ptr() as _,
            ptr::null(),
            &mut startup_info.StartupInfo,
            &mut process_information,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }

    Ok(process_information.dwProcessId)
}

#[must_use]
#[tracing::instrument(level = "debug")]
fn detach(build_root: &Path) -> bool {
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            use std::{fs::File, os::unix::prelude::*};

            // Redirect stderr to file
            let stderr_file = match std::fs::OpenOptions::new().create(true).truncate(true).write(true).open(build_root.join("server.stderr")) {
                Ok(f) => f,
                Err(error) => {
                    eprintln!("failed to open stderr redirection file: {}", error);
                    return false;
                }
            };

            if let Err(err) = replace_fd(stderr_file.as_raw_fd(), 2) {
                eprintln!("failed to replace stderr: {}", err);
                return false;
            }

            // Close stdin/stdout and send to null
            for &fd in [0, 1].iter() {
                let replacement_file = File::open("/dev/null").unwrap();
                if let Err(err) = replace_fd(replacement_file.as_raw_fd(), fd) {
                    eprintln!("failed to replace stdout/stdin: {}", err);
                    return false;
                }
            }

            // Split into a new session & process group
            let session_id = unsafe { libc::setsid() };
            if session_id < 0 {
                let error = std::io::Error::last_os_error();
                error!(code = "setsid_failed", error = error_value(&error));
                eprintln!("failed to detach process for parent session: {}", error);
                return false;
            }
            debug!(code = "setsid", new_session_id = session_id);
            true
        } else if #[cfg(target_os = "windows")] {
            use std::{fs::File, os::windows::prelude::*};

            use winapi::um::{processenv::SetStdHandle, winbase::STD_ERROR_HANDLE};

            // Redirect stderr to file
            let stderr_file = match File::create(build_root.join("server.stderr")) {
                Ok(f) => f,
                Err(error) => {
                    eprintln!("failed to open stderr redirection file: {}", error);
                    return false;
                }
            };

            if unsafe { SetStdHandle(STD_ERROR_HANDLE, stderr_file.into_raw_handle()) } == 0 {
                eprintln!("failed to replace stderr: {}", io::Error::last_os_error());
                return false;
            }

            true
        } else {
            compile_error!("unsupported target os");
        }
    }
}

#[cfg(unix)]
fn replace_fd(old_fd: std::os::unix::prelude::RawFd, new_fd: std::os::unix::prelude::RawFd) -> io::Result<()> {
    use cealn_core::{fs::unix::set_cloexec, libc_call};

    cfg_if::cfg_if! {
        if #[cfg(target_os = "linux")] {
            unsafe { libc_call!(libc::dup3(old_fd, new_fd, libc::O_CLOEXEC))? };
            Ok(())
        } else {
            unsafe { libc_call!(libc::dup2(old_fd, new_fd))? };
            set_cloexec(new_fd)?;
            Ok(())
        }
    }
}
