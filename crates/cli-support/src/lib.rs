#![feature(let_chains)]

pub mod console;
pub mod logging;

use std::{env, fs, path::PathBuf};

use anyhow::{bail, Result};
use cealn_client::{Client, ClientOptions};
use cealn_core::{
    files::{workspace_file_exists_in, WellKnownFileError},
    trace_call_result,
};
use clap::Parser;

use cealn_data::{reference::Reference, rule::BuildConfig, Label, LabelBuf};
use convert_case::{Case, Casing};
use target_lexicon::{Architecture, Triple};

#[derive(Parser, Debug)]
pub struct ClientOpts {
    /// The root source directory
    ///
    /// If not specified, this is the *topmost* directory containing a `workspace.cealn` file in the current working
    /// directory's parent chain. This means it is safe to run cealn in a nested workspace; the workspace root will
    /// not be affected, but the default workspace will be (allowing specifying labels in the current workspace with
    /// `//mypackage:mytarget` notation).
    #[clap(long)]
    workspace_root: Option<PathBuf>,

    /// The root directory containing all files generated as part of the build
    ///
    /// The build root indentifies a server context; precisely one server may be running at a time for a given build
    /// root. If not specified, the "cealn-build" folder in the workspace root is used.
    #[clap(long)]
    build_root: Option<PathBuf>,

    /// The default package label to use with relative labels (e.g. `//mypackage:mytarget` or `:mytarget`)
    ///
    /// If not specified, this is the *bottommost* directory containing a `build.cealn` or `workspace.cealn` file in
    /// the current working directory's parent chain.
    #[clap(long)]
    default_package: Option<LabelBuf>,

    /// Indicate whether output should be displayed in active terminal mode or not
    ///
    /// By default this is determined automatically by detecting whether stderr is attached to a tty on Unix
    /// or the Console on Windows.
    #[clap(long)]
    terminal: Option<bool>,

    #[clap(long)]
    jobs: Option<usize>,
}

#[tracing::instrument(level = "debug", err)]
pub async fn create_client(client_opts: &ClientOpts) -> Result<Client> {
    let mut workspace_root = match &client_opts.workspace_root {
        Some(p) => p.clone(),
        None => find_root_workspace()?,
    };
    // We leave the workspace_root as an absolute but un-canonicalized path so we can report more useful paths in
    // messages, but we use the canonical version for determining the build root.
    if workspace_root.is_relative() {
        workspace_root = trace_call_result!(env::current_dir())?.join(&workspace_root);
    }
    let canonical_workspace_root = trace_call_result!(fs::canonicalize(&workspace_root))?;

    let build_root = match &client_opts.build_root {
        Some(p) => p.clone(),
        None => {
            let mut build_root = canonical_workspace_root;
            build_root.push("cealn-build");
            build_root
        }
    };

    // Calculate default package based on current directory of invocation
    let default_package = match &client_opts.default_package {
        Some(buf) => Some(buf.clone()),
        None => match trace_call_result!(env::current_dir())?.strip_prefix(&workspace_root) {
            Ok(relative_path) => match Label::from_native_relative_path(relative_path) {
                Ok(relative_label) => Some(
                    Label::new("//")
                        .unwrap()
                        .join(&relative_label)
                        .unwrap()
                        .normalize()
                        .unwrap()
                        .into_owned(),
                ),
                Err(err) => bail!("invalid filename in working directory path: {err}"),
            },
            // Current directory is outside of workspace root, just use no default package
            Err(_err) => None,
        },
    };

    let client = Client::launch_or_connect(
        &workspace_root,
        &build_root,
        ClientOptions {
            default_package,
            jobs: client_opts.jobs,
        },
    )
    .await?;
    Ok(client)
}

impl ClientOpts {
    pub fn should_use_terminal(&self) -> bool {
        if let Some(terminal) = self.terminal {
            return terminal;
        }

        atty::is(atty::Stream::Stderr)
    }
}

#[tracing::instrument(level = "debug", err)]
pub fn find_root_workspace() -> Result<PathBuf> {
    // Find root workspace and default package by walking current directory tree
    let mut current_dir = trace_call_result!(env::current_dir())?;

    let mut bottommost_workspace_dir: Option<PathBuf> = None;
    let mut bottommost_workspace_dir_wrong_case: Option<PathBuf> = None;
    loop {
        match workspace_file_exists_in(&current_dir)? {
            Ok(()) => {
                bottommost_workspace_dir = Some(current_dir.clone());
            }
            Err(WellKnownFileError::ExistsWithDifferentCase) => {
                bottommost_workspace_dir_wrong_case = Some(current_dir.clone());
            }
            _ => {}
        }
        if !current_dir.pop() {
            break;
        }
    }

    match (bottommost_workspace_dir, bottommost_workspace_dir_wrong_case) {
        (Some(bottommost_workspace_dir), _) => Ok(bottommost_workspace_dir),
        (None, Some(bottommost_workspace_dir_wrong_case)) => {
            bail!("unable to find root workspace, but found workspace file with incorrect case in {:?}. note that 'workspace.cealn' files must be named in all lowercase.", bottommost_workspace_dir_wrong_case)
        }
        (None, None) => bail!("unable to find root workspace"),
    }
}

pub fn host_build_config(opt: bool) -> BuildConfig {
    let mut options = vec![
        (
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: "Os".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: match std::env::consts::OS {
                    "unknown" => "UnknownOs".to_owned(),
                    other => other.to_case(Case::Pascal),
                },
            },
        ),
        (
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: "Vendor".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: match std::env::consts::OS {
                    "windows" => "Pc".to_owned(),
                    _ => "UnknownVendor".to_owned(),
                },
            },
        ),
        (
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: "Arch".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: match std::env::consts::ARCH {
                    "x86_64" => "X86_64".to_owned(),
                    other => other.to_case(Case::Pascal),
                },
            },
        ),
    ];

    let mut host_options = options.clone();

    if opt {
        options.push((
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
                qualname: "CompilationMode".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
                qualname: "Optimized".to_owned(),
            },
        ));
    } else {
        options.push((
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
                qualname: "CompilationMode".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
                qualname: "Fastbuild".to_owned(),
            },
        ));
    }
    host_options.push((
        Reference {
            source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
            qualname: "CompilationMode".to_owned(),
        },
        Reference {
            source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
            qualname: "Optimized".to_owned(),
        },
    ));

    BuildConfig { options, host_options }
}

pub fn triple_build_config(triple: &Triple, opt: bool) -> BuildConfig {
    let base = host_build_config(opt);

    let mut options = vec![
        (
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: "Os".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: match &triple.operating_system {
                    target_lexicon::OperatingSystem::Unknown => "UnknownOs".to_owned(),
                    other => other.to_string().to_case(Case::Pascal),
                },
            },
        ),
        (
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: "Vendor".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: match &triple.vendor {
                    target_lexicon::Vendor::Unknown => "UnknownVendor".to_owned(),
                    other => other.to_string().to_case(Case::Pascal),
                },
            },
        ),
        (
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: "Arch".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:platform.py").unwrap(),
                qualname: match triple.architecture {
                    Architecture::X86_64 => "X86_64".to_owned(),
                    other => other.to_string().to_case(Case::Pascal),
                },
            },
        ),
    ];

    if opt {
        options.push((
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
                qualname: "CompilationMode".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
                qualname: "Optimized".to_owned(),
            },
        ));
    } else {
        options.push((
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
                qualname: "CompilationMode".to_owned(),
            },
            Reference {
                source_label: LabelBuf::new("@com.cealn.builtin//:config.py").unwrap(),
                qualname: "Fastbuild".to_owned(),
            },
        ));
    }

    BuildConfig {
        options,
        host_options: base.host_options,
    }
}
