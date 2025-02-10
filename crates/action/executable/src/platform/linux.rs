#[path = "linux/fuse.rs"]
pub(crate) mod fuse;
#[path = "linux/interceptor.rs"]
mod interceptor;
#[path = "linux/namespaces.rs"]
pub(crate) mod namespaces;

use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    ffi::{c_int, c_ulong, CString, NulError, OsString},
    fmt::Write as _,
    fs::{self, DirEntry},
    io::{self, BufRead, BufReader, Read},
    mem,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            prelude::{OsStrExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::ExitStatus,
    ptr,
    time::Duration,
};

use anyhow::{bail, Context as AnyhowContext};
use cealn_depset::{depmap, ConcreteFiletree, ConcreteFiletreeBuilder, DepMap};
use compio_core::buffer::AllowTake;
use compio_fs::{os::linux::FileExt, Directory, File, OpenOptions};
use futures::{channel::oneshot, FutureExt};
use regex::Regex;

use cealn_action_context::Context;
use cealn_data::{
    action::{
        ActionOutput, ArgumentSource, Executable, ExecutePlatform, JsonPath, LinuxExecutePlatform, Run,
        StructuredMessageConfig, StructuredMessageLevel,
    },
    depmap::ConcreteFiletreeType,
    file_entry::{FileEntry, FileEntryRef},
    label::{LabelPath, LabelPathBuf},
};
use cealn_event::EventContext;
use cealn_fs::Cachefile;
use cealn_protocol::{
    event::BuildEventData,
    query::{StdioLine, StdioStreamType},
};
use tempfile::TempDir;

use crate::{
    platform::{
        fuse::CACHE_MOUNT_PATH,
        namespaces::{sys, BindMount, Namespaces, NamespacesParams, PidFd},
    },
    stdio::emit_events_for_line,
    Result,
};

pub async fn run<C>(context: &C, action: &Run<ConcreteFiletreeType>) -> Result<ActionOutput>
where
    C: Context,
{
    let action_platform = match &action.platform {
        ExecutePlatform::Linux(linux) => linux,
        ExecutePlatform::MacOS(_) => bail!("cannot execute macOS executables on this platform"),
    };

    let mut events = context.events().fork();

    let ticket = context.acquire_process_ticket().await?;

    events.send(BuildEventData::QueryRunStart);

    let scratch_dir = context.tempdir("cealn_runner_scratch").await?;
    let materialize_cache = context.materialize_cache();
    let mut overlay_bind_mounts = Vec::new();
    let mut direct_bind_mounts = Vec::new();
    let mut fuse_mounts = Vec::new();

    // Build execution root
    let sysroot_depmap = context
        .lookup_concrete_depmap_force_directory(&action_platform.execution_sysroot)
        .await?;
    let sysroot_materialized = materialize_cache.materialize(sysroot_depmap).await?;
    let mut sysroot_lower_dirs = Vec::new();
    {
        // Mount underneath overlay mounts
        // FIXME: I'm not sure the order here is correct
        let mut overlay_stack: Vec<_> = sysroot_materialized.overlays().iter().collect();
        while let Some(overlay) = overlay_stack.pop() {
            if !overlay.dest_subpath.is_empty() {
                continue;
            }
            let overlay_path = overlay.materialized.direct_path().to_owned();
            if sysroot_lower_dirs.iter().any(|x| x == &overlay_path) {
                // If the directory is already in the tree at a higher level, don't add it again. Doing so is wasteful
                // and may cause in ELOOP in some kernels
                continue;
            }
            sysroot_lower_dirs.push(overlay_path);
            overlay_stack.extend(overlay.materialized.overlays().iter());
        }
        sysroot_lower_dirs.push(sysroot_materialized.direct_path().to_owned());
        for overlay in sysroot_materialized.overlays() {
            if overlay.dest_subpath.is_empty() {
                continue;
            }
            todo!()
        }
    }

    // Build executable context
    let executable_context_dirty_dir = scratch_dir.path().join("exec-dirty");
    let executable_context_work_dir = scratch_dir.path().join("exec-work");
    fs::create_dir_all(&executable_context_dirty_dir)?;
    fs::create_dir_all(&executable_context_work_dir)?;
    let executable_context_materialized;
    if let Some(executable_context) = &action.executable.context {
        let executable_context_depmap = context
            .lookup_concrete_depmap_force_directory(executable_context)
            .await?;
        executable_context_materialized = materialize_cache.materialize(executable_context_depmap).await?;
        let mount_dir = PathBuf::from(&action_platform.execution_sysroot_exec_context_dest);

        // Mount underneath overlay mounts
        // FIXME: I'm not sure the order here is correct
        let mut root_lowerdirs = vec![executable_context_materialized.direct_path().to_owned()];
        let mut overlay_stack: Vec<_> = executable_context_materialized.overlays().iter().collect();
        while let Some(overlay) = overlay_stack.pop() {
            if !overlay.dest_subpath.is_empty() {
                continue;
            }
            let overlay_path = overlay.materialized.direct_path().to_owned();
            if root_lowerdirs.iter().any(|x| x == &overlay_path) {
                // If the directory is already in the tree at a higher level, don't add it again. Doing so is wasteful
                // and may cause in ELOOP in some kernels
                continue;
            }
            root_lowerdirs.push(overlay_path);
            overlay_stack.extend(overlay.materialized.overlays().iter());
        }

        overlay_bind_mounts.push(namespaces::Overlay {
            mount_dir: mount_dir.clone(),
            lower_dir: root_lowerdirs,
            upper_dir: None,
            work_dir: None,
        });
        // Mount over-top overlay mounts
        for overlay in executable_context_materialized.overlays() {
            if overlay.dest_subpath.is_empty() {
                continue;
            }
            todo!()
        }
    }

    // Build input
    let output_dir = scratch_dir.path().join("output");
    let output_work_dir = scratch_dir.path().join("output-work");
    Directory::create_all(&output_dir).await?;
    Directory::create_all(&output_work_dir).await?;
    let input;
    let input_materialized;
    if let Some(input_ref) = &action.input {
        input = context.lookup_concrete_depmap_force_directory(&input_ref).await?;
        let mount_dir = PathBuf::from(&action_platform.execution_sysroot_input_dest);

        if action_platform.use_fuse {
            let lowerdir = scratch_dir.path().join("input-fuse");
            Directory::create_all(&lowerdir).await?;
            fuse_mounts.push(namespaces::FuseMount {
                mount_dir: lowerdir.clone(),
                mount_callback: Box::new({
                    let context = context.clone();
                    let input = input.clone();
                    move |mount_path| {
                        let mount = fuse::mount_depmap(&context, input.clone(), &mount_path)?;
                        Ok(mount)
                    }
                }),
            });
            overlay_bind_mounts.push(namespaces::Overlay {
                mount_dir: mount_dir.clone(),
                lower_dir: vec![lowerdir],
                upper_dir: Some(output_dir.clone()),
                work_dir: Some(output_work_dir.clone()),
            });
        } else {
            input_materialized = materialize_cache.materialize(input.clone()).await?;

            // Mount underneath overlay mounts
            // FIXME: I'm not sure the order here is correct
            let mut root_lowerdirs = vec![input_materialized.direct_path().to_owned()];
            let mut overlay_stack: Vec<_> = input_materialized.overlays().iter().collect();
            while let Some(overlay) = overlay_stack.pop() {
                if !overlay.dest_subpath.is_empty() {
                    continue;
                }
                let overlay_path = overlay.materialized.direct_path().to_owned();
                if root_lowerdirs.iter().any(|x| x == &overlay_path) {
                    // If the directory is already in the tree at a higher level, don't add it again. Doing so is wasteful
                    // and may cause in ELOOP in some kernels
                    continue;
                }
                root_lowerdirs.push(overlay_path);
                overlay_stack.extend(overlay.materialized.overlays().iter());
            }
            overlay_bind_mounts.push(namespaces::Overlay {
                mount_dir: mount_dir.clone(),
                lower_dir: root_lowerdirs,
                upper_dir: Some(output_dir.clone()),
                work_dir: Some(output_work_dir.clone()),
            });
            // Mount over-top overlay mounts
            for overlay in input_materialized.overlays() {
                if overlay.dest_subpath.is_empty() {
                    continue;
                }
                todo!()
            }
        }
    } else {
        input = Default::default();
        let empty_input_dir = scratch_dir.path().join("input");
        Directory::create_all(&empty_input_dir).await?;
        overlay_bind_mounts.push(namespaces::Overlay {
            mount_dir: PathBuf::from(&action_platform.execution_sysroot_input_dest),
            lower_dir: vec![empty_input_dir],
            upper_dir: Some(output_dir.clone()),
            work_dir: Some(output_work_dir),
        });
    }
    direct_bind_mounts.push(BindMount {
        mount_dir: action_platform.execution_sysroot_output_dest.clone().into(),
        source_dir: output_dir.clone(),
    });

    // Bind mount cache directory
    if action_platform.use_fuse {
        direct_bind_mounts.push(BindMount {
            mount_dir: CACHE_MOUNT_PATH.into(),
            source_dir: context.primary_cache_dir().to_owned(),
        });
    }

    // Figure out executable parameters
    let spawned_executable_path;
    let mut generated_respfile_index = 0usize;
    let mut argv: Vec<OsString> = Vec::new();

    spawned_executable_path = PathBuf::from(substitute_vars(action_platform, &action.executable.executable_path));
    argv.push(spawned_executable_path.clone().into());
    for arg in &action.args {
        match arg {
            ArgumentSource::Literal(value) => argv.push(OsString::from(substitute_vars(action_platform, value))),
            ArgumentSource::Label(reference) => {
                let depmap = context.lookup_concrete_depmap_force_directory(reference).await?;
                for entry in depmap.iter() {
                    let (k, _) = entry?;
                    argv.push(OsString::from(k.as_str()));
                }
            }
            ArgumentSource::Templated { template, source } => {
                let depmap = context.lookup_concrete_depmap_force_directory(source).await?;
                let mut path_set = BTreeSet::new();
                for entry in depmap.iter() {
                    let (k, _) = entry?;
                    if !path_set.contains(k.as_str()) {
                        let substituted = template.replace("$1", k.as_str());
                        argv.push(OsString::from(substituted));
                        path_set.insert(k.as_str().to_owned());
                    }
                }
            }
            ArgumentSource::Respfile { template, source } => {
                let respfile_name = format!(".cealn-generated-respfile-{}.resp", generated_respfile_index);
                let respfile_path = Path::new(&action_platform.execution_sysroot_input_dest).join(&respfile_name);

                let mut respfile_contents = String::new();
                let mut respfile_set = BTreeSet::new();
                let depmap = context.lookup_concrete_depmap_force_directory(source).await?;
                for entry in depmap.iter() {
                    let (k, _) = entry?;
                    if !respfile_set.contains(k.as_str()) {
                        writeln!(&mut respfile_contents, "{}", k.as_str()).unwrap();
                        respfile_set.insert(k.as_str().to_owned());
                    }
                }
                {
                    let mut respfile = File::create(output_dir.join(&respfile_name)).await?;
                    respfile.write_all_mono(&mut respfile_contents.into_bytes()).await?;
                }

                let substituted = template.replace("$1", respfile_path.to_str().unwrap());
                argv.push(OsString::from(substituted));
                generated_respfile_index += 1;
            }
        }
    }
    let mut envs = Vec::new();
    let mut path_components = Vec::new();
    let mut ld_path_components = Vec::new();
    // FIXME: handle windows path separator
    let pathsep = ":";
    for (k, v) in &action_platform.standard_environment_variables {
        let v = substitute_vars(action_platform, v);
        if k == "PATH" {
            path_components.extend(v.split(pathsep).map(|x| x.to_owned()));
            continue;
        }
        if k == "LD_LIBRARY_PATH" {
            ld_path_components.extend(v.split(pathsep).map(|x| x.to_owned()));
            continue;
        }
        envs.push((OsString::from(k), OsString::from(v)));
    }
    // FIXME: handle join properly
    path_components.splice(
        0..0,
        action
            .executable
            .search_paths
            .iter()
            .map(|v| substitute_vars(action_platform, v))
            .map(|subpath| format!("{}/{}", action_platform.execution_sysroot_exec_context_dest, subpath)),
    );
    // FIXME: handle join properly
    ld_path_components.splice(
        0..0,
        action
            .executable
            .library_search_paths
            .iter()
            .map(|v| substitute_vars(action_platform, v))
            .map(|subpath| format!("{}/{}", action_platform.execution_sysroot_exec_context_dest, subpath)),
    );
    for (k, v) in &action.append_env {
        let v = substitute_vars(action_platform, v);
        if k == "PATH" {
            path_components.extend(v.split(pathsep).map(|x| x.to_owned()));
            continue;
        }
        if k == "LD_LIBRARY_PATH" {
            ld_path_components.extend(v.split(pathsep).map(|x| x.to_owned()));
            continue;
        }
        envs.push((OsString::from(k), OsString::from(v)));
    }
    for envfile_ref in &action.append_env_files {
        let envfile = context.open_depmap_file(envfile_ref).await?;
        let mut envfile = File::open(&*envfile).await?;
        let mut envfile_data = Vec::new();
        envfile.read_to_end(AllowTake(&mut envfile_data)).await?;
        for line in envfile_data.split(|x| *x == b'\n') {
            if line.is_empty() {
                continue;
            }
            let mut split_var = line.splitn(2, |x| *x == b'=');
            let k = std::str::from_utf8(split_var.next().unwrap())?;
            let v = split_var.next().context("missing '=' in envfile line")?;
            let v = std::str::from_utf8(v)?;
            let v = substitute_vars(action_platform, v);
            if k == "PATH" {
                path_components.extend(v.split(pathsep).map(|x| x.to_owned()));
                continue;
            }
            if k == "LD_LIBRARY_PATH" {
                ld_path_components.extend(v.split(pathsep).map(|x| x.to_owned()));
                continue;
            }
            envs.push((OsString::from(k), OsString::from(v)));
        }
    }
    envs.push((OsString::from("PATH"), OsString::from(path_components.join(pathsep))));
    envs.push((
        OsString::from("LD_LIBRARY_PATH"),
        OsString::from(ld_path_components.join(pathsep)),
    ));
    if action_platform.use_interceptor {
        let pid = unsafe { libc::getpid() };
        let injection_subdir = context.tempdir_root().join(format!("cealn-injection-{pid}"));
        if !injection_subdir.exists() {
            fs::create_dir_all(&injection_subdir)?;
            let interceptor_so = injection_subdir.join("libcealn_interceptor.so");
            fs::write(&interceptor_so, interceptor::CEALN_INTERCEPTOR_BYTES)?;
            fs::set_permissions(&interceptor_so, fs::Permissions::from_mode(0o555))?;
        }
        direct_bind_mounts.push(BindMount {
            mount_dir: PathBuf::from(interceptor::INJECTION_PATH).parent().unwrap().to_owned(),
            source_dir: injection_subdir,
        });
        envs.push((
            OsString::from("LD_PRELOAD"),
            OsString::from(interceptor::INJECTION_PATH),
        ));
    }
    let workdir = action
        .cwd
        .as_deref()
        .map(|cwd| substitute_vars(action_platform, cwd))
        .unwrap_or_else(|| action_platform.execution_sysroot_input_dest.clone());
    let mut workdir = PathBuf::from(workdir);
    if workdir.is_relative() {
        workdir = Path::new(&action_platform.execution_sysroot_input_dest).join(workdir);
    }

    let mut command_friendly_string = String::new();
    for arg in &argv {
        if let Some(arg) = arg.to_str() {
            if let Some(FileEntryRef::Regular {
                content_hash,
                executable,
            }) = arg
                .strip_prefix("@")
                .and_then(|resp_filename| {
                    workdir
                        .strip_prefix(&action_platform.execution_sysroot_input_dest)
                        .ok()
                        .map(|workdir| workdir.join(resp_filename))
                })
                .and_then(|path| path.to_str().map(|x| x.to_owned()))
                .and_then(|path| LabelPathBuf::new(path).ok())
                .and_then(|path| path.normalize_require_descending().map(|x| x.into_owned()))
                .and_then(|path| input.get(path.as_ref()).unwrap())
            {
                // Response file
                let contents = context.open_cache_file(content_hash, executable).await?;
                let reader = BufReader::new(std::fs::File::open(&*contents)?);
                for line in reader.lines() {
                    let line = line?;
                    if !command_friendly_string.is_empty() {
                        command_friendly_string.push(' ');
                    }
                    write!(
                        &mut command_friendly_string,
                        "{}",
                        shell_escape::unix::escape(Cow::Owned(line))
                    )
                    .unwrap();
                }
            } else {
                if !command_friendly_string.is_empty() {
                    command_friendly_string.push(' ');
                }
                write!(
                    &mut command_friendly_string,
                    "{}",
                    shell_escape::unix::escape(Cow::Borrowed(arg))
                )
                .unwrap();
            }
        } else {
            if !command_friendly_string.is_empty() {
                command_friendly_string.push(' ');
            }
            write!(&mut command_friendly_string, "{:?}", arg).unwrap();
        }
    }

    let mut extra_file_contents = BTreeMap::new();
    extra_file_contents.insert(
        PathBuf::from("/etc/resolv.conf"),
        compio_fs::read("/etc/resolv.conf").await?,
    );

    let mut stdout = context.tempfile("stdout", false).await?;
    let mut stderr = context.tempfile("stderr", false).await?;
    let stdout_read = reopen(&mut stdout, OpenOptions::new().read(true)).await?;
    let stderr_read = reopen(&mut stderr, OpenOptions::new().read(true)).await?;

    let mut namespaces = Namespaces::new(NamespacesParams {
        executable_path: spawned_executable_path,

        argv,
        envs,
        workdir,

        uid: action_platform.uid,
        gid: action_platform.gid,

        stdout: Some(clone_fd(&mut stdout).await?),
        stderr: Some(clone_fd(&mut stderr).await?),

        scratch_dir: scratch_dir.path().join("namespace_scratch"),
        sysroot_lower_dirs,
        overlay_bind_mounts,
        direct_bind_mounts,
        fuse_mounts,
        bind_proc: false,
        bind_dev: false,
        extra_file_contents,
    });

    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let shutdown_rx = shutdown_rx.shared();

    let stdout_relay = if !action.hide_stdout {
        Some(context.spawn_immediate({
            let events = context.events().fork();
            let shutdown_rx = shutdown_rx.clone();
            let structured_messages = action.structured_messages.clone();
            async move {
                tail_file(
                    stdout_read,
                    events,
                    StdioStreamType::Stdout,
                    shutdown_rx,
                    structured_messages,
                )
                .await
            }
        }))
    } else {
        None
    };

    let stderr_relay = if !action.hide_stderr {
        Some(context.spawn_immediate({
            let events = context.events().fork();
            let shutdown_rx = shutdown_rx.clone();
            let structured_messages = action.structured_messages.clone();
            async move {
                tail_file(
                    stderr_read,
                    events,
                    StdioStreamType::Stderr,
                    shutdown_rx,
                    structured_messages,
                )
                .await
            }
        }))
    } else {
        None
    };

    let status = namespaces.run().await;

    mem::drop(ticket);
    mem::drop(namespaces);

    let _ = shutdown_tx.send(());
    if let Some(stdout_relay) = stdout_relay {
        stdout_relay.await?;
    }
    if let Some(stderr_relay) = stderr_relay {
        stderr_relay.await?;
    }

    let status = status.with_context(|| command_friendly_string.clone())?;

    if !status.success() {
        bail!("command exited with status {}\n{}", status, command_friendly_string);
    }

    let mut output_files = ConcreteFiletree::builder();
    load_depmap(context, &output_dir, &mut output_files).await?;
    let output_files = context.register_concrete_filetree_depmap(output_files.build()).await?;

    let (stdout, _) = context.move_to_cache(stdout).await?;
    let (stderr, _) = context.move_to_cache(stderr).await?;

    tokio::task::spawn_blocking(move || {
        // FIXME: report errors
        let _ = scratch_dir.close();
    });

    Ok(ActionOutput {
        files: output_files,
        stdout: Some(stdout),
        stderr: Some(stderr),
    })
}

pub(crate) struct PreparedRunGuard {
    pid_fd: PidFd,
    scratch_dir: TempDir,
}

pub(crate) async fn prepare_for_run<'a, C>(
    context: &'a C,
    executable: &'a Executable<ConcreteFiletreeType>,
    platform: &'a ExecutePlatform<ConcreteFiletreeType>,
    source_root: &'a Path,
) -> Result<PreparedRunGuard>
where
    C: Context,
{
    let action_platform = match platform {
        ExecutePlatform::Linux(linux) => linux,
        ExecutePlatform::MacOS(_) => bail!("cannot execute macOS executables on this platform"),
    };

    let scratch_dir = context.tempdir("cealn_runner_scratch").await?;
    let materialize_cache = context.materialize_cache();
    let mut overlay_bind_mounts = Vec::new();

    // Build execution root
    let sysroot_depmap = context
        .lookup_concrete_depmap_force_directory(&action_platform.execution_sysroot)
        .await?;
    let sysroot_materialized = materialize_cache.materialize(sysroot_depmap).await?;
    let mut sysroot_lower_dirs = Vec::new();
    {
        // Mount underneath overlay mounts
        // FIXME: I'm not sure the order here is correct
        let mut overlay_stack: Vec<_> = sysroot_materialized.overlays().iter().collect();
        while let Some(overlay) = overlay_stack.pop() {
            if !overlay.dest_subpath.is_empty() {
                continue;
            }
            let overlay_path = overlay.materialized.direct_path().to_owned();
            if sysroot_lower_dirs.iter().any(|x| x == &overlay_path) {
                // If the directory is already in the tree at a higher level, don't add it again. Doing so is wasteful
                // and may cause in ELOOP in some kernels
                continue;
            }
            sysroot_lower_dirs.push(overlay_path);
            overlay_stack.extend(overlay.materialized.overlays().iter());
        }
        sysroot_lower_dirs.push(sysroot_materialized.direct_path().to_owned());
        for overlay in sysroot_materialized.overlays() {
            if overlay.dest_subpath.is_empty() {
                continue;
            }
            todo!()
        }
    }

    // Build executable context
    let executable_context_dirty_dir = scratch_dir.path().join("exec-dirty");
    let executable_context_work_dir = scratch_dir.path().join("exec-work");
    fs::create_dir_all(&executable_context_dirty_dir)?;
    fs::create_dir_all(&executable_context_work_dir)?;
    let executable_context_materialized;
    if let Some(executable_context) = &executable.context {
        let executable_context_depmap = context
            .lookup_concrete_depmap_force_directory(executable_context)
            .await?;
        executable_context_materialized = materialize_cache.materialize(executable_context_depmap).await?;
        let mount_dir = PathBuf::from(&action_platform.execution_sysroot_exec_context_dest);

        // Mount underneath overlay mounts
        // FIXME: I'm not sure the order here is correct
        let mut root_lowerdirs = vec![executable_context_materialized.direct_path().to_owned()];
        let mut overlay_stack: Vec<_> = executable_context_materialized.overlays().iter().collect();
        while let Some(overlay) = overlay_stack.pop() {
            if !overlay.dest_subpath.is_empty() {
                continue;
            }
            let overlay_path = overlay.materialized.direct_path().to_owned();
            if root_lowerdirs.iter().any(|x| x == &overlay_path) {
                // If the directory is already in the tree at a higher level, don't add it again. Doing so is wasteful
                // and may cause in ELOOP in some kernels
                continue;
            }
            root_lowerdirs.push(overlay_path);
            overlay_stack.extend(overlay.materialized.overlays().iter());
        }

        overlay_bind_mounts.push(namespaces::Overlay {
            mount_dir: mount_dir.clone(),
            lower_dir: root_lowerdirs,
            upper_dir: None,
            work_dir: None,
        });
        // Mount over-top overlay mounts
        for overlay in executable_context_materialized.overlays() {
            if overlay.dest_subpath.is_empty() {
                continue;
            }
            todo!()
        }
    }

    let mut extra_file_contents = BTreeMap::new();
    extra_file_contents.insert(
        PathBuf::from("/etc/resolv.conf"),
        compio_fs::read("/etc/resolv.conf").await?,
    );

    let namespaces = Namespaces::new(NamespacesParams {
        // FIXME: configure
        executable_path: PathBuf::from("/bin/sh"),

        argv: vec![
            OsString::from("/bin/sh"),
            OsString::from("-c"),
            OsString::from("while true; do sleep 600; done"),
        ],
        envs: Default::default(),
        workdir: PathBuf::from("/"),

        uid: action_platform.uid,
        gid: action_platform.gid,

        stdout: None,
        stderr: None,

        scratch_dir: scratch_dir.path().join("namespace_scratch"),
        sysroot_lower_dirs,
        overlay_bind_mounts,
        direct_bind_mounts: vec![
            BindMount {
                mount_dir: action_platform.execution_sysroot_input_dest.clone().into(),
                source_dir: source_root.to_owned(),
            },
            BindMount {
                mount_dir: "/home".into(),
                source_dir: "/home".into(),
            },
            BindMount {
                mount_dir: "/host".into(),
                source_dir: "/".into(),
            },
        ],
        fuse_mounts: Default::default(),
        bind_proc: true,
        bind_dev: true,
        extra_file_contents,
    });

    let pid_fd = namespaces.launch_pause().await?;

    Ok(PreparedRunGuard { pid_fd, scratch_dir })
}

impl PreparedRunGuard {
    pub fn parent_pid(&self) -> u32 {
        self.pid_fd.pid as u32
    }
}

pub fn enter_prepared(parent_pid: u32, executable_path: &Path, args: &[OsString], workdir: &Path) -> Result<u32> {
    unsafe {
        let executable_path_cstr = CString::new(executable_path.as_os_str().as_bytes())?;
        let argv = args
            .iter()
            .map(|x| CString::new(x.as_bytes()))
            .collect::<Result<Vec<_>, NulError>>()?;
        let mut argv_ptrs: Vec<_> = argv.iter().map(|x| x.as_ptr()).collect();
        argv_ptrs.insert(0, executable_path_cstr.as_ptr());
        argv_ptrs.push(ptr::null());
        let workdir_cstr = CString::new(workdir.as_os_str().as_bytes()).unwrap();

        let ret = sys::pidfd_open(parent_pid as c_int, libc::PIDFD_NONBLOCK);
        if ret < 0 {
            return Err(io::Error::from_raw_os_error((-ret) as i32).into());
        }
        let pidfd = ret as c_int;

        if libc::setns(pidfd, libc::CLONE_NEWUSER) < 0 {
            return Err(anyhow::Error::from(io::Error::last_os_error()).context("failed to enter user namespace"));
        }
        if libc::setns(pidfd, libc::CLONE_NEWNS) < 0 {
            return Err(anyhow::Error::from(io::Error::last_os_error()).context("failed to enter mount namespace"));
        }
        libc::chdir(workdir_cstr.as_ptr());

        // FIXME: source these from config
        std::env::set_var(
            "PATH",
            "/exec/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/exec/.dotnet:/exec/.dotnet/tools",
        );
        std::env::set_var("LD_LIBRARY_PATH", "/host/usr/lib/wsl/lib");
        std::env::set_var("DOTNET_ROOT", "/exec/.dotnet");

        libc::close(pidfd);
        libc::execv(executable_path_cstr.as_ptr(), argv_ptrs.as_ptr());
        return Err(io::Error::last_os_error().into());
    }
}

#[tracing::instrument(level = "info", skip(context, depmap))]
async fn load_depmap<C>(context: &C, source_root: &Path, depmap: &mut ConcreteFiletreeBuilder) -> anyhow::Result<()>
where
    C: Context,
{
    let mut entries = source_root
        .read_dir()?
        .map(|x| x.map(ExpandedEntry::from))
        .collect::<Result<Vec<_>, _>>()?;
    // Sort entries to ensure determinism
    entries.sort_by(|x, y| y.path.file_name().cmp(&x.path.file_name()));

    while let Some(entry) = entries.pop() {
        let Some(subpath) = entry.path.strip_prefix(source_root).ok().and_then(|x| x.to_str()).and_then(|x| LabelPath::new(x).ok()) else {
            // Ignore non-utf8 filenames
            continue;
        };
        let subpath = subpath
            .require_normalized_descending()
            .expect("we formed this path by joining so it should already be normalized descending");

        if subpath.as_str().starts_with(".cealn-generated-respfile-") {
            continue;
        }

        match entry.entry.file_type()? {
            ty if ty.is_dir() => {
                depmap.insert(subpath, FileEntryRef::Directory);

                // Add entries to stack
                let orig_entries_len = entries.len();
                for entry in entry.path.read_dir()? {
                    entries.push(ExpandedEntry::from(entry?));
                }
                // Sort entries to ensure determinism
                entries[orig_entries_len..].sort_by(|x, y| y.path.file_name().cmp(&x.path.file_name()));
            }
            ty if ty.is_symlink() => {
                let link_target = fs::read_link(&entry.path)?;
                let Some(link_target) = link_target.to_str() else {
                    // Ignore non-utf8 filenames
                    continue;
                };
                depmap.insert(subpath, FileEntryRef::Symlink(link_target));
            }
            ty if ty.is_file() => {
                let (content_hash, executable) = context.move_to_cache_named(&entry.path).await?;
                depmap.insert(
                    subpath,
                    FileEntry::Regular {
                        content_hash,
                        executable,
                    }
                    .as_ref(),
                );
            }
            _ => {}
        }
    }

    Ok(())
}

struct ExpandedEntry {
    entry: DirEntry,
    path: PathBuf,
}

impl From<DirEntry> for ExpandedEntry {
    fn from(entry: DirEntry) -> Self {
        ExpandedEntry {
            path: entry.path(),
            entry,
        }
    }
}

async fn tail_file(
    mut file: File,
    mut events: EventContext,
    stream: StdioStreamType,
    mut done_rx: futures::future::Shared<oneshot::Receiver<()>>,
    structured_messages: Option<StructuredMessageConfig>,
) -> anyhow::Result<()> {
    let mut buffer = Vec::with_capacity(128 * 1024);
    let mut shutting_down = false;
    loop {
        if buffer.capacity() == buffer.len() {
            buffer.reserve(64);
        }

        let read_count = file.read(AllowTake(&mut buffer)).await?;

        if read_count == 0 {
            if shutting_down {
                break;
            }
            match tokio::time::timeout(Duration::from_millis(100), &mut done_rx).await {
                Ok(_) => {
                    shutting_down = true;
                }
                Err(_) => continue,
            }
        }

        let consumed_len = {
            let mut buffer_remaining = &*buffer;
            while let Some(offset) = memchr::memchr(b'\n', &buffer_remaining) {
                let (line, tail) = buffer_remaining.split_at(offset);
                buffer_remaining = &tail[1..];

                emit_events_for_line(&mut events, stream, structured_messages.as_ref(), line);
            }
            buffer.len() - buffer_remaining.len()
        };
        buffer.drain(..consumed_len);
    }
    if !buffer.is_empty() {
        // Emit last data regardless of line content
        events.send(BuildEventData::Stdio {
            line: StdioLine {
                stream,
                contents: buffer,
            },
        });
    }

    Ok(())
}

async fn reopen(cachefile: &mut Cachefile, options: &mut OpenOptions) -> anyhow::Result<File> {
    let orig_file = cachefile.ensure_open().await?;
    let file = options.open(format!("/proc/self/fd/{}", orig_file.as_raw_fd())).await?;
    Ok(file)
}

async fn clone_fd(cachefile: &mut Cachefile) -> anyhow::Result<std::fs::File> {
    unsafe {
        let orig_file = cachefile.ensure_open().await?;
        let new_fd = libc::fcntl(orig_file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0);
        if new_fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let file = std::fs::File::from_raw_fd(new_fd);
        Ok(file)
    }
}

fn substitute_vars<'s>(action_platform: &LinuxExecutePlatform<ConcreteFiletreeType>, value: &str) -> String {
    SUBSTITUTE_REGEX
        .replace_all(&value, VarsReplacer { action_platform })
        .into_owned()
}

lazy_static::lazy_static! {
    static ref SUBSTITUTE_REGEX: Regex = Regex::new(r#"(?x)
        %\[(?P<name> srcdir | execdir )\]
    "#).unwrap();
}

struct VarsReplacer<'a> {
    action_platform: &'a LinuxExecutePlatform<ConcreteFiletreeType>,
}

impl regex::Replacer for VarsReplacer<'_> {
    fn replace_append(&mut self, caps: &regex::Captures<'_>, dst: &mut String) {
        match caps.name("name").unwrap().as_str() {
            "srcdir" => dst.push_str(&self.action_platform.execution_sysroot_input_dest),
            "execdir" => dst.push_str(&self.action_platform.execution_sysroot_exec_context_dest),
            _ => unreachable!(),
        }
    }
}
