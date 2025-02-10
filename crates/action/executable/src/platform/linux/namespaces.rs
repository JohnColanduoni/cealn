#[path = "namespaces/sys.rs"]
pub(crate) mod sys;

use anyhow::{anyhow, bail, Context, Result};
use bumpalo::Bump;
use cealn_core::libc_call;
use compio_core::os::linux::Poller;
use libc::{c_char, c_int, c_uint};
use regex::Regex;
use tracing::{event_enabled, info_span, Instrument, error, Level, Span};

use std::{
    cell::UnsafeCell,
    collections::BTreeMap,
    ffi::{CStr, CString, OsStr, OsString},
    fmt::Write as _,
    fs::{self, File},
    io::{self, BufRead, BufReader, Read, Write},
    mem,
    os::{linux::fs::MetadataExt as LinuxMetadataExt, raw::c_ulong, unix::prelude::*},
    panic,
    path::{Path, PathBuf},
    process::ExitStatus,
    ptr,
    sync::{
        atomic::{AtomicI32, AtomicIsize, Ordering},
        Arc, Barrier,
    },
    thread::{self, Thread},
    time::Duration,
};

use crate::platform::fuse;

pub(crate) struct Namespaces {
    shared: Arc<Shared>,
    leader: Option<PidFd>,
}

impl Drop for Namespaces {
    fn drop(&mut self) {
        if let Some(leader) = self.leader.take() {
            unsafe {
                sys::pidfd_send_signal(leader.fd, libc::SIGTERM, ptr::null_mut(), 0);
            }
        }
    }
}

struct Shared {
    params: NamespacesParams,

    caller_uid: u32,
    caller_gid: u32,

    debug_fork: bool,
}

/// Required parameters for setting up namespaces
pub(crate) struct NamespacesParams {
    pub executable_path: PathBuf,
    pub argv: Vec<OsString>,
    pub envs: Vec<(OsString, OsString)>,
    pub workdir: PathBuf,

    pub uid: u32,
    pub gid: u32,

    pub stdout: Option<File>,
    pub stderr: Option<File>,

    pub scratch_dir: PathBuf,

    pub sysroot_lower_dirs: Vec<PathBuf>,
    pub overlay_bind_mounts: Vec<Overlay>,
    pub direct_bind_mounts: Vec<BindMount>,
    pub fuse_mounts: Vec<FuseMount>,
    pub bind_proc: bool,
    pub bind_dev: bool,

    pub extra_file_contents: BTreeMap<PathBuf, Vec<u8>>,
}

pub struct Overlay {
    pub mount_dir: PathBuf,

    pub upper_dir: Option<PathBuf>,
    pub lower_dir: Vec<PathBuf>,
    pub work_dir: Option<PathBuf>,
}

pub struct BindMount {
    pub mount_dir: PathBuf,
    pub source_dir: PathBuf,
}

pub struct FuseMount {
    pub mount_dir: PathBuf,
    pub mount_callback: Box<dyn Fn(PathBuf) -> anyhow::Result<fuse::Mount> + Send + Sync>,
}

const PIVOT_ROOT_DIRNAME: &str = ".pivot_root";

impl Namespaces {
    pub(crate) fn new(params: NamespacesParams) -> Namespaces {
        let caller_uid = unsafe { libc::getuid() };
        let caller_gid = unsafe { libc::getgid() };

        Namespaces {
            shared: Arc::new(Shared {
                params,
                caller_uid,
                caller_gid,

                debug_fork: event_enabled!(Level::TRACE),
            }),
            leader: None,
        }
    }

    pub(crate) async fn run(&mut self) -> Result<ExitStatus> {
        unsafe {
            let leader_pid_fd = self.shared.prepare_namespace_leader()?;
            self.leader = Some(leader_pid_fd);
            let leader_pid_fd = self.leader.as_ref().unwrap();

            let pid_fd = self.launch(&leader_pid_fd)?;

            let span = info_span!("execute");
            async {
                let poller = Poller::new(pid_fd.fd)?;
                poller.wait_for_read().await
            }
            .instrument(span)
            .await?;

            let mut infop: libc::siginfo_t = mem::zeroed();
            let ret = libc::waitid(libc::P_PIDFD, pid_fd.fd as c_uint, &mut infop, libc::WEXITED);
            if ret < 0 {
                return Err(io::Error::last_os_error().into());
            }

            match infop.si_code {
                libc::CLD_EXITED => Ok(ExitStatus::from_raw(libc::W_EXITCODE(infop.si_status(), 0))),
                libc::CLD_KILLED => Ok(ExitStatus::from_raw(libc::W_EXITCODE(0, infop.si_status()))),
                _ => bail!("unknown si_code"),
            }
        }
    }

    pub(crate) async fn launch_pause(&self) -> Result<PidFd> {
        let leader_pid_fd = self.shared.prepare_namespace_leader()?;
        Ok(leader_pid_fd)
    }

    fn launch(&self, leader_pid_fd: &PidFd) -> Result<PidFd> {
        unsafe {
            let arena = Bump::new();
            let executable_path = CString::new(self.shared.params.executable_path.as_os_str().as_bytes())?;
            let argv_raw = arena.alloc_slice_fill_copy(self.shared.params.argv.len() + 1, ptr::null());
            for (arg, arg_raw) in self.shared.params.argv.iter().zip(argv_raw.iter_mut()) {
                *arg_raw = make_cstr(&arena, arg)?.as_ptr();
            }
            debug_assert!(argv_raw[argv_raw.len() - 1].is_null());
            let envs_raw = arena.alloc_slice_fill_copy(self.shared.params.envs.len() + 1, ptr::null());
            let mut path_var = None;
            for ((k, v), env_raw) in self.shared.params.envs.iter().zip(envs_raw.iter_mut()) {
                if k == OsStr::new("PATH") {
                    path_var = Some(make_cstr(&arena, v)?);
                }
                let mut combined = OsString::with_capacity(k.len() + 1 + v.len());
                combined.push(k);
                combined.push("=");
                combined.push(v);
                *env_raw = make_cstr(&arena, &combined)?.as_ptr();
            }
            debug_assert!(envs_raw[envs_raw.len() - 1].is_null());
            let work_dir_cstr = CString::new(self.shared.params.workdir.as_os_str().as_bytes())?;
            let path_var = path_var.unwrap_or_else(|| CStr::from_bytes_with_nul(b"\0").unwrap());

            // Use pipe to receive any pre-exec errors from cloned process. The pipe will automatically disconnect
            // when the exec completes succesfully (since we set cloexec).
            let mut status_pipe: [c_int; 2] = [0, 0];
            if libc::pipe2(status_pipe.as_mut_ptr(), libc::O_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error().into());
            }
            let mut status_pipe_read = File::from_raw_fd(status_pipe[0]);
            let status_pipe_write = File::from_raw_fd(status_pipe[1]);

            let child_pidfd = AtomicI32::new(-1);
            let child_pid = AtomicI32::new(-1);
            let mut clone_args: libc::clone_args = mem::zeroed();
            clone_args.flags = (libc::CLONE_VM | libc::CLONE_VFORK | libc::CLONE_FILES) as u64;
            let clone_ret = sys::clone3(&mut clone_args, mem::size_of_val(&clone_args))?;
            match clone_ret {
                0 => self.child(
                    status_pipe_write.as_raw_fd(),
                    leader_pid_fd,
                    &executable_path,
                    &argv_raw,
                    &envs_raw,
                    path_var.as_ptr(),
                    &work_dir_cstr,
                    &child_pidfd,
                    &child_pid,
                ),
                _child_pid => {}
            }

            let pidfd = PidFd {
                fd: child_pidfd.load(Ordering::SeqCst),
                pid: child_pid.load(Ordering::SeqCst),
            };

            // Close our copy of the write side of the pipe so it will automatically close when the other side does
            mem::drop(status_pipe_write);

            let mut message_buffer = String::new();
            status_pipe_read.read_to_string(&mut message_buffer)?;
            if message_buffer.len() > 0 {
                // A pre-exec error ocurred
                bail!(
                    "error ocurred before subprocess {:?} could be executed in namespace:\n{}",
                    self.shared.params.executable_path,
                    message_buffer
                );
            }

            Ok(pidfd)
        }
    }

    unsafe fn child(
        &self,
        status_pipe_write: c_int,
        leader_pid_fd: &PidFd,
        executable_path: &CStr,
        argv: &[*const c_char],
        envs: &[*const c_char],
        path_var: *const c_char,
        workdir: &CStr,
        pidfd_out: &AtomicI32,
        pid_out: &AtomicI32,
    ) -> ! {
        // We're now past a fork in a multithreaded process, so we need to exercise a lot of caution:
        //  * Can't touch any locks
        //  * Therefore no memory allocation or deallocation
        //  * We can call virtually no library functions, since we don't know if they'll lock
        // We make some assumptions that basic Rust library functions (e.g. iterating maps, etc.) do not take locks
        // or unexpectedly allocate. This mostly seems to work okay.

        if libc::setns(leader_pid_fd.fd, libc::CLONE_NEWUSER) < 0 {
            fork_abort(status_pipe_write, "failed to enter user namespace");
        }
        if libc::setns(leader_pid_fd.fd, libc::CLONE_NEWNS | libc::CLONE_NEWPID) < 0 {
            fork_abort(status_pipe_write, "failed to enter namespaces");
        }

        // Fork again so we can really enter the PID namespace
        let mut clone_args: libc::clone_args = mem::zeroed();
        let mut pidfd: libc::pid_t = 0;
        clone_args.flags = (libc::CLONE_VM | libc::CLONE_VFORK | libc::CLONE_PARENT | libc::CLONE_PIDFD) as u64;
        clone_args.pidfd = &mut pidfd as *mut c_int as usize as u64;
        let clone_ret = sys::clone3(&mut clone_args, mem::size_of_val(&clone_args));
        match clone_ret {
            Ok(0) => {
                let stdin = libc::open(b"/dev/null\0".as_ptr() as _, libc::O_RDONLY);
                if stdin < 0 {
                    fork_abort(status_pipe_write, "failed to open stdin");
                }
                if stdin != 0 {
                    libc::dup2(stdin, 0);
                    libc::close(stdin);
                }

                if let Some(stdout) = &self.shared.params.stdout {
                    libc::dup2(stdout.as_raw_fd(), 1);
                    libc::close(stdout.as_raw_fd());
                }
                if let Some(stderr) = &self.shared.params.stderr {
                    libc::dup2(stderr.as_raw_fd(), 2);
                    libc::close(stderr.as_raw_fd());
                }

                if libc::chdir(workdir.as_ptr()) < 0 {
                    fork_abort(status_pipe_write, "failed to set workdir");
                }

                libc::syscall(libc::SYS_close_range, 3, c_int::MAX, libc::CLOSE_RANGE_CLOEXEC);

                // Set path environment variable for execvpe to search
                libc::setenv(b"PATH\0".as_ptr() as _, path_var, 1);

                libc::execvpe(executable_path.as_ptr(), argv.as_ptr(), envs.as_ptr());

                // If we get here, terminate with an abort to ensure no stack unwinding happens
                fork_abort(status_pipe_write, "failed to launch process")
            }
            Ok(child_pid) => {
                pidfd_out.store(pidfd, Ordering::SeqCst);
                pid_out.store(child_pid, Ordering::SeqCst);
                libc::syscall(libc::SYS_exit, 0);
                std::intrinsics::abort();
            }
            Err(_) => {
                fork_abort(status_pipe_write, "failed to clone");
            }
        }
    }

    fn get_pivoted_path(&self, path: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
        let path = path.as_ref();
        let root_relative_path = path
            .strip_prefix("/")
            .with_context(|| format!("relative path provided to namespace system: {:?}", path))?;
        Ok(Path::new("/").join(PIVOT_ROOT_DIRNAME).join(root_relative_path))
    }
}

unsafe fn fork_abort(status_pipe_write: c_int, message: &str) -> ! {
    let errno = *libc::__errno_location();
    libc::write(status_pipe_write, message.as_ptr() as _, message.len());
    let mut errno_buffer = heapless::String::<32>::new();
    let _ = write!(&mut errno_buffer, " (errno {})", errno);
    libc::write(
        status_pipe_write,
        errno_buffer.as_bytes().as_ptr() as _,
        errno_buffer.len(),
    );
    libc::syscall(libc::SYS_exit, 1);
    std::intrinsics::abort()
}

impl Shared {
    fn prepare_namespace_leader(self: &Arc<Self>) -> Result<PidFd> {
        unsafe {
            let parent_span = Span::current();

            let mut status_pipe: [c_int; 2] = [0, 0];
            if libc::pipe2(status_pipe.as_mut_ptr(), libc::O_CLOEXEC) < 0 {
                return Err(io::Error::last_os_error().into());
            }
            let mut status_pipe_read = File::from_raw_fd(status_pipe[0]);
            let status_pipe_write = File::from_raw_fd(status_pipe[1]);

            let host_thread = thread::Builder::new().name("cealn-ns".to_string()).spawn({
                let this = self.clone();
                move || this.host_namespace_leader(status_pipe_write, parent_span)
            })?;

            // Wait for initialization to complete
            let mut buffer = Vec::new();
            status_pipe_read.read_to_end(&mut buffer)?;

            if buffer.is_empty() {
                match host_thread.join() {
                    Ok(Ok(())) => unreachable!(),
                    Ok(Err(err)) => return Err(err),
                    Err(panic_payload) => {
                        panic::resume_unwind(panic_payload);
                    }
                }
            }

            let pidfd = c_int::from_str_radix(std::str::from_utf8(&buffer).unwrap(), 10).unwrap();

            let fdinfo = fs::read_to_string(format!("/proc/self/fdinfo/{}", pidfd))?;
            let fdinfo_pid_match = FDINFO_PID_REGEX
                .captures(&fdinfo)
                .context("failed to parse fdinfo for pidfd")?;
            let pid = libc::pid_t::from_str_radix(fdinfo_pid_match.name("pid").unwrap().as_str(), 10)?;

            Ok(PidFd { fd: pidfd, pid })
        }
    }

    fn host_namespace_leader(&self, status_pipe_write: File, parent_span: Span) -> Result<()> {
        unsafe {
            let mut error_out = None;
            let mut clone_args: libc::clone_args = mem::zeroed();
            let mut pidfd_out: libc::c_int = 0;
            clone_args.flags = (libc::CLONE_NEWUSER
                | libc::CLONE_NEWNS
                | libc::CLONE_NEWPID
                | libc::CLONE_VM
                | libc::CLONE_VFORK
                | libc::CLONE_FILES
                | libc::CLONE_PIDFD) as u64;
            clone_args.pidfd = &mut pidfd_out as *mut libc::c_int as usize as u64;
            match sys::clone3(&mut clone_args, mem::size_of_val(&clone_args)) {
                Ok(0) => match self.do_namespace_leader(status_pipe_write, parent_span) {
                    Ok(()) => {
                        libc::syscall(libc::SYS_exit_group, 0);
                        std::intrinsics::abort()
                    }
                    Err(err) => {
                        eprintln!("namespace leader error: {:?}", err);
                        error_out = Some(err);
                        libc::syscall(libc::SYS_exit_group, 1);
                        std::intrinsics::abort();
                    }
                },
                Ok(child_pid) => {
                    // Child shares our file desciptor table and has likely already closed the pipe
                    mem::forget(status_pipe_write);
                    mem::forget(parent_span);
                    let pidfd = PidFd {
                        pid: child_pid,
                        fd: pidfd_out,
                    };
                    let mut child_exit_info: libc::siginfo_t = mem::zeroed();
                    match libc::waitid(libc::P_PIDFD, pidfd.fd as u32, &mut child_exit_info, libc::WNOHANG | libc::WEXITED) {
                        0 => {
                            // Child already exited
                            let exit_status = match child_exit_info.si_code {
                                libc::CLD_EXITED => ExitStatus::from_raw(libc::W_EXITCODE(child_exit_info.si_status(), 0)),
                                libc::CLD_KILLED => ExitStatus::from_raw(libc::W_EXITCODE(0, child_exit_info.si_status())),
                                _ => bail!("unknown si_code"),
                            };
                            if exit_status.success() {
                                return Ok(());
                            } 
                        },
                        _ => {
                            error!("waitid failed on namespace leader: {}", io::Error::last_os_error());
                        }
                    }
                    if let Some(err) = error_out.take() {
                        return Err(err);
                    } else {
                        bail!("namespace leader failed but no error was set")
                    }
                }
                Err(_err) => todo!(),
            }
        }
    }

    unsafe fn do_namespace_leader(&self, mut status_pipe_write: File, parent_span: Span) -> Result<()> {
        let span = info_span!(parent: &parent_span, "namespace_leader");
        let _guard = span.enter();

        libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL);

        // Setup UID and GID map, then drop to non-root user (still with admin caps however)
        // This needs to be done before any filesystem manipulations, as otherwise the VFS won't know what to do with
        // our UID/GID on disk
        fs::write(
            "/proc/self/uid_map",
            format!(
                "{container_uid} {outer_uid} 1\n",
                container_uid = self.params.uid,
                outer_uid = self.caller_uid,
            ),
        ).context("/proc/self/uid_map write failed")?;
        fs::write("/proc/self/setgroups", "deny").context("/proc/self/setgroups write failed")?;
        fs::write(
            "/proc/self/gid_map",
            format!(
                "{container_gid} {outer_gid} 1\n",
                container_gid = self.params.gid,
                outer_gid = self.caller_gid,
            ),
        ).context("/proc/self/gid_map write failed")?;

        // Mount new root directory so we can set it up
        fs::create_dir_all(&self.params.scratch_dir)?;
        let scratch_dir_cstr = CString::new(self.params.scratch_dir.as_os_str().as_bytes())?;
        libc_call!(libc::mount(
            ptr::null(),
            scratch_dir_cstr.as_ptr(),
            "tmpfs\0".as_ptr() as _,
            libc::MS_NOATIME,
            b"\0".as_ptr() as _,
        ))?;
        libc_call!(libc::mount(
            ptr::null(),
            scratch_dir_cstr.as_ptr(),
            ptr::null(),
            libc::MS_PRIVATE,
            ptr::null()
        ))?;

        let root_dest = self.params.scratch_dir.join("sysroot");
        let sysroot_upper_dir = self.params.scratch_dir.join("sysroot-upper");
        let sysroot_work_dir = self.params.scratch_dir.join("sysroot-upper-work");
        fs::create_dir_all(&root_dest)?;
        fs::create_dir_all(&sysroot_upper_dir)?;
        fs::create_dir_all(&sysroot_work_dir)?;
        self.mount_overlayfs(
            &root_dest,
            &self.params.sysroot_lower_dirs,
            Some(&sysroot_upper_dir),
            Some(&sysroot_work_dir),
        )?;

        // Pivot to our new root
        let pivot_root_dir = root_dest.join(PIVOT_ROOT_DIRNAME);
        fs::create_dir_all(&pivot_root_dir)?;
        let root_dest_cstr = CString::new(root_dest.as_os_str().as_bytes())?;
        let pivot_root_dir_cstr = CString::new(pivot_root_dir.as_os_str().as_bytes())?;
        libc_call!(sys::pivot_root(root_dest_cstr.as_ptr(), pivot_root_dir_cstr.as_ptr()))?;

        // Setup /dev
        fs::create_dir_all("/dev")?;
        if self.params.bind_dev {
            self.mount_bind("/.pivot_root/dev", "/dev", false).unwrap();
        } else {
            let dev_cstr = CString::new("/dev")?;
            libc_call!(libc::mount(
                ptr::null(),
                dev_cstr.as_ptr(),
                "tmpfs\0".as_ptr() as _,
                libc::MS_NOATIME,
                // Mode must be set to this value to allow mknod
                b"mode=0775\0".as_ptr() as _,
            ))?;
            libc_call!(libc::mount(
                ptr::null(),
                dev_cstr.as_ptr(),
                ptr::null(),
                libc::MS_PRIVATE,
                ptr::null()
            ))?;
            for dev_file in &["null", "zero"] {
                let metadata = fs::metadata(Path::new("/.pivot_root/dev").join(dev_file))?;
                let dev_file_path_cstr = CString::new(Path::new("/dev").join(dev_file).into_os_string().into_vec())?;
                libc_call!(libc::mknod(dev_file_path_cstr.as_ptr(), 0o666, metadata.st_rdev()))?;
            }
            for dev_file in &["random", "urandom", "fuse"] {
                let dest_path = Path::new("/dev").join(dev_file);
                let src_path = Path::new("/.pivot_root/dev").join(dev_file);
                fs::write(&dest_path, &b""[..])?;
                self.mount_bind(&src_path, &dest_path, false)?;
            }
        }

        // Mount procfs
        fs::create_dir_all("/proc")?;
        if self.params.bind_proc {
            self.mount_bind("/.pivot_root/proc", "/proc", false)?;
        } else {
            libc_call!(libc::mount(
                "proc\0".as_ptr() as _,
                b"/proc\0".as_ptr() as _,
                "proc\0".as_ptr() as _,
                0,
                ptr::null_mut()
            ))?;
        }

        // Mount sysfs
        fs::create_dir_all("/sys")?;
        self.mount_bind("/.pivot_root/sys", "/sys", false)?;

        // FIXME: this is a hack to get dotnet to run okay inside container. Figure out why it refuses to do this itself
        fs::create_dir_all("/tmp/.dotnet")?;

        // Mount fuse mounts
        let mut fuse_mounts = Vec::new();
        for fuse_mount_spec in &self.params.fuse_mounts {
            let pivoted_mount_path = self.get_pivoted_path(&fuse_mount_spec.mount_dir)?;
            let mount = (fuse_mount_spec.mount_callback)(pivoted_mount_path)?;
            fuse_mounts.push(mount);
        }

        // Mount overlays
        for (overlay_index, overlay) in self.params.overlay_bind_mounts.iter().enumerate() {
            let mut lowerdir_paths_pivoted: Vec<PathBuf> = overlay
                .lower_dir
                .iter()
                .map(|lower_dir| self.get_pivoted_path(lower_dir))
                .collect::<anyhow::Result<Vec<PathBuf>>>()?;
            let upperdir_path_pivoted = match &overlay.upper_dir {
                Some(path) => self.get_pivoted_path(path)?,
                None => {
                    let upperdir_path_pivoted = self
                        .get_pivoted_path(&self.params.scratch_dir.join(format!("overlay-upper-{}", overlay_index)))?;
                    fs::create_dir_all(&upperdir_path_pivoted)?;
                    upperdir_path_pivoted
                }
            };
            let workdir_path_pivoted = match &overlay.work_dir {
                Some(path) => self.get_pivoted_path(path)?,
                None => {
                    let workdir_path_pivoted = self.get_pivoted_path(
                        &self
                            .params
                            .scratch_dir
                            .join(format!("overlay-workdir-{}", overlay_index)),
                    )?;
                    fs::create_dir_all(&workdir_path_pivoted).with_context(|| format!("failed to create overlay workdir for mount {:?}", overlay.mount_dir))?;
                    workdir_path_pivoted
                }
            };
            fs::create_dir_all(&overlay.mount_dir).with_context(|| format!("failed to create overlay mount directory {:?}", overlay.mount_dir))?;

            // Mount options are truncated at 4096 bytes, including the null terminator. To prevent going over this
            // limit, we create readonly overlayfs mounts and aggregate them in the final list
            let mut aggregate_index = 0;
            let mut aggregate_paths = Vec::new();
            while self
                .build_overlayfs_options(
                    &lowerdir_paths_pivoted,
                    Some(&upperdir_path_pivoted),
                    Some(&workdir_path_pivoted),
                )
                .len()
                > 4095
            {
                // FIXME: don't just guess here
                let shed_paths: Vec<_> = lowerdir_paths_pivoted
                    .drain((lowerdir_paths_pivoted.len() - 4)..)
                    .collect();
                let intermediate_mount_dir = self.get_pivoted_path(
                    self.params
                        .scratch_dir
                        .join(format!("overlay-aggregate-{}-{}", overlay_index, aggregate_index,)),
                )?;
                fs::create_dir_all(&intermediate_mount_dir)?;
                self.mount_overlayfs(&intermediate_mount_dir, &shed_paths, None::<PathBuf>, None::<PathBuf>)?;
                aggregate_paths.insert(0, intermediate_mount_dir);

                aggregate_index += 1;
            }
            lowerdir_paths_pivoted.extend(aggregate_paths.drain(..));

            self.mount_overlayfs(
                &overlay.mount_dir,
                &lowerdir_paths_pivoted,
                Some(upperdir_path_pivoted),
                Some(workdir_path_pivoted),
            ).with_context(|| format!("failed to mount overlay at {:?}", overlay.mount_dir))?;
        }

        // Mount bind mounts
        for bind_mount in &self.params.direct_bind_mounts {
            fs::create_dir_all(&bind_mount.mount_dir)?;
            self.mount_bind(
                &self.get_pivoted_path(&bind_mount.source_dir)?,
                &bind_mount.mount_dir,
                false,
            )?;
        }

        // Finally completely unmount our original root
        let pivot_root_cstr = CString::new("/.pivot_root")?;
        libc_call!(libc::umount2(pivot_root_cstr.as_ptr(), libc::MNT_DETACH))?;
        fs::remove_dir("/.pivot_root")?;

        // Add extra files
        for (path, contents) in &self.params.extra_file_contents {
            fs::write(path, contents.clone())?;
        }

        // Change directory to source location
        fs::create_dir_all(&self.params.workdir)?;

        // Setup signals
        unsafe extern "C" fn signal_handler(_signum: c_int) {}

        libc::signal(libc::SIGTERM, signal_handler as usize);
        libc::signal(libc::SIGCHLD, signal_handler as usize);

        let ret = sys::pidfd_open(1, 0);
        if ret < 0 {
            bail!("pidfd_open failed");
        }
        write!(&mut status_pipe_write, "{}", ret)?;
        mem::drop(status_pipe_write);

        let mut signal_mask: libc::sigset_t = mem::zeroed();
        libc::sigemptyset(&mut signal_mask);
        libc::sigaddset(&mut signal_mask, libc::SIGTERM);
        libc::sigaddset(&mut signal_mask, libc::SIGCHLD);
        loop {
            let mut siginfo: libc::siginfo_t = mem::zeroed();
            libc_call!(libc::sigwaitinfo(&signal_mask, &mut siginfo))?;
            match siginfo.si_signo as c_int {
                libc::SIGCHLD => {
                    // Reap process
                    let mut child_siginfo: libc::siginfo_t = mem::zeroed();
                    libc::waitid(libc::P_ALL, 0, &mut child_siginfo, libc::WNOHANG);
                }
                libc::SIGTERM => {
                    break;
                }
                _ => unreachable!(),
            }
        }

        for fuse_mount in fuse_mounts {
            fuse_mount.shutdown()?;
        }

        Ok(())
    }

    fn mount_bind(&self, src: impl AsRef<Path>, dest: impl AsRef<Path>, readonly: bool) -> Result<()> {
        unsafe {
            let mut flags = libc::MS_BIND | libc::MS_REC | libc::MS_NOSUID | libc::MS_PRIVATE;
            if readonly {
                flags |= libc::MS_RDONLY;
            }
            let src_cstr = CString::new(src.as_ref().as_os_str().as_bytes())?;
            let dest_cstr = CString::new(dest.as_ref().as_os_str().as_bytes())?;
            libc_call!(libc::mount(
                src_cstr.as_ptr(),
                dest_cstr.as_ptr(),
                ptr::null(),
                flags,
                ptr::null_mut()
            ))?;
            Ok(())
        }
    }

    fn mount_overlayfs(
        &self,
        mount_dir: impl AsRef<Path>,
        lowerdirs: &[impl AsRef<Path>],
        upperdir: Option<impl AsRef<Path>>,
        workdir: Option<impl AsRef<Path>>,
    ) -> Result<()> {
        unsafe {
            let mount_dir_cstr = CString::new(mount_dir.as_ref().as_os_str().as_bytes())?;
            let options = self.build_overlayfs_options(lowerdirs, upperdir, workdir);
            let options_cstr = CString::new(options.into_vec())?;
            libc_call!(libc::mount(
                b"overlay\0".as_ptr() as _,
                mount_dir_cstr.as_ptr(),
                b"overlay\0".as_ptr() as _,
                libc::MS_NOATIME,
                options_cstr.as_ptr() as _,
            ))?;
            libc_call!(libc::mount(
                ptr::null(),
                mount_dir_cstr.as_ptr(),
                ptr::null(),
                libc::MS_PRIVATE,
                ptr::null()
            ))?;
            Ok(())
        }
    }

    fn build_overlayfs_options(
        &self,
        lowerdirs: &[impl AsRef<Path>],
        upperdir: Option<impl AsRef<Path>>,
        workdir: Option<impl AsRef<Path>>,
    ) -> OsString {
        let mut options = OsString::new();
        options.push("lowerdir=");
        let mut first_lowerdir = true;
        for lowerdir in lowerdirs {
            if !first_lowerdir {
                options.push(":");
            } else {
                first_lowerdir = false;
            }
            options.push(lowerdir.as_ref());
        }
        if let Some(upperdir) = upperdir {
            options.push(",upperdir=");
            options.push(upperdir.as_ref());
        }
        if let Some(workdir) = workdir {
            options.push(",workdir=");
            options.push(workdir.as_ref());
        }
        options.push(",volatile,userxattr");
        options
    }

    fn get_pivoted_path(&self, path: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
        let path = path.as_ref();
        let root_relative_path = path
            .strip_prefix("/")
            .with_context(|| format!("relative path provided to namespace system: {:?}", path))?;
        Ok(Path::new("/").join(PIVOT_ROOT_DIRNAME).join(root_relative_path))
    }
}

fn make_cstr<'a>(arena: &'a Bump, contents: impl AsRef<OsStr>) -> anyhow::Result<&'a CStr> {
    let contents = contents.as_ref().as_bytes();
    if memchr::memchr(0u8, contents).is_some() {
        bail!("null in string");
    }
    let slice = arena.alloc_slice_fill_copy(contents.len() + 1, 0u8);
    slice[..contents.len()].copy_from_slice(contents);
    unsafe { Ok(CStr::from_bytes_with_nul_unchecked(slice)) }
}

lazy_static::lazy_static! {
    static ref NSTGID_REGEX: Regex = Regex::new(r#"^NStgid:(\s+(?P<pid>\d+))+?(\s+(?P<ns_pid>\d+))$"#).unwrap();
}

lazy_static::lazy_static! {
    static ref FDINFO_PID_REGEX: Regex = Regex::new(r#"(?m)^Pid:\s*(?P<pid>\d+)$"#).unwrap();
}

pub(crate) struct PidFd {
    pub(crate) fd: c_int,
    pub(crate) pid: c_int,
}

impl Drop for PidFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}

struct SignalFd {
    fd: c_int,
}

impl Drop for SignalFd {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}
