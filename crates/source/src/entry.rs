use std::{
    cell::OnceCell,
    collections::{BTreeMap, BTreeSet, HashMap},
    fmt,
    fs::{self, File},
    io::{self, BufRead, BufReader},
    mem,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    task::Waker,
    time::{Instant, SystemTime},
};

use anyhow::{bail, Context, Result};
use dashmap::{mapref::entry::Entry, DashMap};
use parking_lot::Mutex;
use ring::digest::{self, SHA256};
use tokio::sync::{RwLock, RwLockReadGuard};

use cealn_core::fs::{FileNodeIdentifier, MetadataExt};
use cealn_data::{
    file_entry::FileHash,
    label::{LabelPath, LabelPathBuf},
    Label, LabelBuf,
};

use crate::{source_monitor, watcher::WatchNode};

pub(super) struct SourceEntryMonitor {
    full_file_path: PathBuf,
    // This will be a workspace-relative path
    root_relative_path: LabelBuf,

    pub(crate) shared: Arc<source_monitor::Shared>,

    state: RwLock<Option<State>>,
    parent_watch_node: Option<WatchNode>,

    watch_wakers: Mutex<Vec<Waker>>,
}

#[derive(Clone, Debug)]
pub enum Status {
    Directory(DirectoryStatus),
    File(FileStatus),
    Symlink(SymlinkStatus),
    NotFound,
}

struct DirectoryState {
    last_observed: DirectoryStatus,
}

#[derive(Clone, Debug)]
pub struct DirectoryStatus {
    pub mtime: SystemTime,
    pub file_node_identifier: FileNodeIdentifier,

    pub children: BTreeSet<LabelPathBuf>,
}

struct FileState {
    last_observed: FileStatus,
}

#[derive(Clone, Debug)]
pub struct FileStatus {
    pub mtime: SystemTime,
    pub file_node_identifier: FileNodeIdentifier,
    pub hash: FileHash,
    pub executable: bool,
}

struct SymlinkState {
    last_observed: SymlinkStatus,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SymlinkStatus {
    pub mtime: SystemTime,
    pub file_node_identifier: FileNodeIdentifier,
    pub target: String,
}

struct State {
    observed_at: spin::Mutex<Instant>,
    kind: StateKind,
    watch_node: Option<WatchNode>,
    monitored_children: OnceLock<DashMap<LabelBuf, Arc<SourceEntryMonitor>>>,
}

enum StateKind {
    File(FileState),
    Directory(DirectoryState),
    Symlink(SymlinkState),
    NotFound,
}

impl SourceEntryMonitor {
    pub fn new_root(
        canonical_workspace_root: PathBuf,
        shared: Arc<source_monitor::Shared>,
    ) -> Result<Arc<SourceEntryMonitor>> {
        let observed_at = Instant::now();
        let metadata = fs::symlink_metadata(&canonical_workspace_root)?;

        if !metadata.is_dir() {
            bail!("workspace root is not a directory: {:?}", canonical_workspace_root);
        }

        let children = enumerate_directory(&canonical_workspace_root, Label::new("//").unwrap(), &shared)?;

        let monitor = Arc::new(SourceEntryMonitor {
            full_file_path: canonical_workspace_root,
            root_relative_path: LabelBuf::new("//").unwrap(),

            shared,

            state: RwLock::new(None),
            parent_watch_node: None,
            watch_wakers: Default::default(),
        });

        let watch_node = match &monitor.shared.watch_port {
            Some(watch_port) => watch_port.watch(&monitor.full_file_path, &monitor)?,
            None => None,
        };

        *monitor.state.try_write().unwrap() = Some(State {
            kind: StateKind::Directory(DirectoryState {
                last_observed: DirectoryStatus {
                    mtime: metadata.modified()?,
                    file_node_identifier: metadata.file_node_identifier()?,
                    children,
                },
            }),
            monitored_children: Default::default(),
            watch_node,
            observed_at: observed_at.into(),
        });

        Ok(monitor)
    }

    fn new(
        full_file_path: PathBuf,
        root_relative_path: LabelBuf,
        shared: Arc<source_monitor::Shared>,
        parent_watch_node: Option<WatchNode>,
    ) -> Result<Arc<SourceEntryMonitor>> {
        debug_assert!(root_relative_path.is_workspace_relative());

        Ok(Arc::new(SourceEntryMonitor {
            full_file_path,
            root_relative_path,

            shared,
            parent_watch_node,

            state: RwLock::new(None),
            watch_wakers: Default::default(),
        }))
    }

    pub async fn current_status(self: &Arc<Self>) -> Result<Status> {
        let guard = self.update_self().await?;
        self.state_to_status(guard.as_ref())
    }

    pub async fn status_until(self: &Arc<Self>, instant: Instant) -> Result<Status> {
        let guard = self.state.read().await;
        if let Some(state) = &*guard {
            if *state.observed_at.lock() >= instant {
                return self.state_to_status(Some(state));
            }
        }
        let guard = self.update_self_with_guard(guard).await?;
        self.state_to_status(guard.as_ref())
    }

    fn state_to_status(&self, state: Option<&State>) -> Result<Status> {
        match state.map(|state| &state.kind) {
            Some(StateKind::Directory(dir_state)) => {
                if self
                    .shared
                    .ignore
                    .matched(&self.root_relative_path.to_native_relative_path().unwrap(), true)
                    .is_ignore()
                {
                    return Ok(Status::NotFound);
                }

                Ok(Status::Directory(dir_state.last_observed.clone()))
            }
            Some(StateKind::File(file_state)) => {
                if self
                    .shared
                    .ignore
                    .matched(&self.root_relative_path.to_native_relative_path().unwrap(), false)
                    .is_ignore()
                {
                    return Ok(Status::NotFound);
                }

                Ok(Status::File(file_state.last_observed.clone()))
            }
            Some(StateKind::Symlink(symlink_state)) => {
                if self
                    .shared
                    .ignore
                    .matched(&self.root_relative_path.to_native_relative_path().unwrap(), false)
                    .is_ignore()
                {
                    return Ok(Status::NotFound);
                }

                Ok(Status::Symlink(symlink_state.last_observed.clone()))
            }
            Some(StateKind::NotFound) => Ok(Status::NotFound),
            None => unreachable!(),
        }
    }

    pub fn full_file_path(&self) -> &Path {
        &self.full_file_path
    }

    pub fn root_relative_path(&self) -> &Label {
        &self.root_relative_path
    }

    pub async fn monitor_child(self: &Arc<Self>, name: &Label) -> Result<Arc<SourceEntryMonitor>> {
        assert_eq!(name.segments().count(), 1, "child should be a single segment");

        let entry = {
            let guard = self.state.read().await;

            // Try updating from disk before we return an error.
            let downgraded_guard;
            let state = match &*guard {
                Some(state) => state,
                _ => {
                    downgraded_guard = self.update_self_with_guard(guard).await?;
                    downgraded_guard.as_ref().unwrap()
                }
            };

            let monitored_children = state.monitored_children.get_or_init(|| Default::default());
            let entry = match monitored_children.entry(name.to_owned()) {
                // TODO: should we update the child here? Or leave that to the caller?
                Entry::Occupied(child) => child.get().clone(),
                Entry::Vacant(slot) => {
                    let entry = SourceEntryMonitor::new(
                        self.full_file_path.join(name.to_native_relative_path().unwrap()),
                        self.root_relative_path.join(name).unwrap(),
                        self.shared.clone(),
                        state.watch_node.as_ref().map(|x| x.duplicate()),
                    )?;
                    slot.insert(entry.clone());
                    entry
                }
            };
            entry
        };

        let _ = entry.update_self().await?;

        Ok(entry)
    }

    async fn update_self<'a>(self: &'a Arc<Self>) -> Result<RwLockReadGuard<'a, Option<State>>> {
        let guard = self.state.read().await;

        self.update_self_with_guard(guard).await
    }

    // Updates this entry's state based on what is currently on disk
    async fn update_self_with_guard<'a>(
        self: &'a Arc<Self>,
        guard: RwLockReadGuard<'a, Option<State>>,
    ) -> Result<RwLockReadGuard<'a, Option<State>>> {
        let mut observe_guard = None;
        if let Some(watch_node) = guard.as_ref().and_then(|x| x.watch_node.as_ref()) {
            if !watch_node.has_changed() {
                return Ok(guard);
            }
            observe_guard = Some(watch_node.will_observe());
        }

        let observed_at = Instant::now();
        let metadata = match compio_fs::symlink_metadata(&self.full_file_path).await {
            Ok(metadata) => metadata,
            Err(ref err) if err.kind() == io::ErrorKind::NotFound || err.kind() == io::ErrorKind::NotADirectory => {
                mem::drop(guard);
                let mut guard = self.state.write().await;
                *guard = Some(State {
                    observed_at: observed_at.into(),
                    monitored_children: Default::default(),
                    watch_node: self.parent_watch_node.as_ref().map(|x| x.duplicate()),
                    kind: StateKind::NotFound,
                });

                return Ok(guard.downgrade());
            }
            Err(err) => return Err(err.into()),
        };

        let mtime = metadata.modified()?;
        let file_node_identifier = metadata.file_node_identifier()?;

        let result = if metadata.is_dir() {
            let up_to_date = match &*guard {
                Some(State {
                    kind: StateKind::Directory(dir),
                    ..
                }) => {
                    dir.last_observed.mtime == mtime && dir.last_observed.file_node_identifier == file_node_identifier
                }
                _ => false,
            };
            if !up_to_date {
                let children = enumerate_directory(&self.full_file_path, &self.root_relative_path, &self.shared)?;

                mem::drop(guard);
                let mut guard = self.state.write().await;
                match &mut *guard {
                    Some(State {
                        kind: StateKind::Directory(dir),
                        observed_at: observed_at_dest,
                        ..
                    }) => {
                        *observed_at_dest.get_mut() = observed_at;
                        dir.last_observed = DirectoryStatus {
                            mtime,
                            file_node_identifier,
                            children,
                        };
                        // FIXME: if file node identifier changes, force all descendants to re-evaluate
                    }
                    state => {
                        let is_ignored = self
                            .shared
                            .ignore
                            .matched(&self.root_relative_path.to_native_relative_path().unwrap(), true)
                            .is_ignore();

                        let watch_node = match &self.shared.watch_port {
                            Some(watch_port) if !is_ignored => watch_port.watch(&self.full_file_path, self)?,
                            _ => None,
                        };

                        *state = Some(State {
                            observed_at: observed_at.into(),
                            kind: StateKind::Directory(DirectoryState {
                                last_observed: DirectoryStatus {
                                    mtime,
                                    file_node_identifier,
                                    children,
                                },
                            }),
                            monitored_children: Default::default(),
                            watch_node,
                        });
                    }
                }
                Ok(guard.downgrade())
            } else {
                *guard.as_ref().unwrap().observed_at.lock() = observed_at;
                Ok(guard)
            }
        } else if metadata.is_file() {
            let up_to_date = match &*guard {
                Some(State {
                    kind: StateKind::File(file),
                    ..
                }) => {
                    file.last_observed.mtime == mtime && file.last_observed.file_node_identifier == file_node_identifier
                }
                _ => false,
            };
            if !up_to_date {
                let status = hash_file(&self.full_file_path, &self.root_relative_path)?;
                mem::drop(guard);
                let mut guard = self.state.write().await;
                match &mut *guard {
                    Some(State {
                        kind: StateKind::File(dir),
                        observed_at: observed_at_dest,
                        ..
                    }) => {
                        *observed_at_dest.get_mut() = observed_at;
                        dir.last_observed = status;
                    }
                    state => {
                        *state = Some(State {
                            kind: StateKind::File(FileState { last_observed: status }),
                            observed_at: observed_at.into(),
                            monitored_children: Default::default(),
                            watch_node: self.parent_watch_node.as_ref().map(|x| x.duplicate()),
                        });
                    }
                }
                Ok(guard.downgrade())
            } else {
                *guard.as_ref().unwrap().observed_at.lock() = observed_at;
                Ok(guard)
            }
        } else if metadata.is_symlink() {
            let up_to_date = match &*guard {
                Some(State {
                    kind: StateKind::Symlink(symlink),
                    ..
                }) => {
                    symlink.last_observed.mtime == mtime
                        && symlink.last_observed.file_node_identifier == file_node_identifier
                }
                _ => false,
            };
            if !up_to_date {
                let target = std::fs::read_link(&self.full_file_path)?;
                let Some(target) = target.to_str() else {
                    todo!();
                };
                let status = SymlinkStatus {
                    mtime,
                    file_node_identifier,
                    target: target.to_owned(),
                };
                mem::drop(guard);
                let mut guard = self.state.write().await;
                match &mut *guard {
                    Some(State {
                        kind: StateKind::Symlink(dir),
                        observed_at: observed_at_dest,
                        ..
                    }) => {
                        *observed_at_dest.get_mut() = observed_at;
                        dir.last_observed = status;
                    }
                    state => {
                        *state = Some(State {
                            kind: StateKind::Symlink(SymlinkState { last_observed: status }),
                            observed_at: observed_at.into(),
                            monitored_children: Default::default(),
                            watch_node: self.parent_watch_node.as_ref().map(|x| x.duplicate()),
                        });
                    }
                }
                Ok(guard.downgrade())
            } else {
                *guard.as_ref().unwrap().observed_at.lock() = observed_at;
                Ok(guard)
            }
        } else {
            // Special file types are ignored by the source directory view, treat this as non-existent
            mem::drop(guard);
            let mut guard = self.state.write().await;
            *guard = Some(State {
                kind: StateKind::NotFound,
                observed_at: observed_at.into(),
                monitored_children: Default::default(),
                watch_node: self.parent_watch_node.as_ref().map(|x| x.duplicate()),
            });
            Ok(guard.downgrade())
        };

        if let Some(observe_guard) = observe_guard {
            if let Ok(guard) = result.as_ref() {
                if let Some(watch_node) = guard.as_ref().and_then(|x| x.watch_node.as_ref()) {
                    observe_guard.commit(watch_node);
                }
            }
        }

        result
    }

    #[inline]
    pub fn wake_on_changed<F>(&self, factory: F) -> bool
    where
        F: FnOnce() -> Waker,
    {
        // FIXME: check if watchable
        let waker = factory();
        let mut wakers = self.watch_wakers.lock();
        wakers.push(waker);
        true
    }

    #[tracing::instrument(level = "debug", skip_all, fields(filename=?self.full_file_path))]
    pub(crate) fn watch_did_change(&self) {
        let wakers = mem::take(&mut *self.watch_wakers.lock());
        for waker in wakers {
            waker.wake();
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(filename=?self.full_file_path))]
    pub(crate) fn watch_dir_did_change(&self) {
        self.watch_did_change();
        let mut state = self.state.blocking_read();
        let Some(state) = &*state else {
            return;
        };
        if let Some(monitored_children) = state.monitored_children.get() {
            for entry in monitored_children.iter() {
                entry.value().watch_did_change();
            }
        }
    }
}

impl Status {
    pub fn equivalent(&self, other: &Status) -> bool {
        match (self, other) {
            (Status::Directory(this), Status::Directory(other)) => this.children == other.children,
            (Status::File(this), Status::File(other)) => this.hash == other.hash && this.executable == other.executable,
            (Status::Symlink(this), Status::Symlink(other)) => this.target == other.target,
            (Status::NotFound, Status::NotFound) => true,
            _ => false,
        }
    }
}

const HASH_BUFFER_SIZE: usize = 64 * 1024;
const HASH_MUTATION_ATTEMPTS: usize = 16;

fn enumerate_directory(
    full_file_path: &Path,
    root_relative_path: &Label,
    shared: &Arc<source_monitor::Shared>,
) -> Result<BTreeSet<LabelPathBuf>> {
    let mut children = BTreeSet::new();
    // TODO: async
    for entry in full_file_path.read_dir()? {
        let entry = entry?;
        let file_name = entry.file_name();
        let Some(file_name) = file_name.to_str() else {
            continue;
        };
        let Ok(file_name) = LabelPath::new(file_name) else {
            continue
        };
        if shared
            .ignore
            .matched(
                &root_relative_path
                    .join(Label::new(file_name.as_str()).unwrap())
                    .unwrap()
                    .to_native_relative_path()
                    .unwrap(),
                entry.file_type()?.is_dir(),
            )
            .is_ignore()
        {
            continue;
        }
        children.insert(file_name.to_owned());
    }
    Ok(children)
}

// Hashes a file, restarting if the file changes in the meantime
fn hash_file(full_file_path: &Path, root_relative_path: &Label) -> Result<FileStatus> {
    // TODO: async
    for _ in 0..HASH_MUTATION_ATTEMPTS {
        // FIXME: handle scheduling IO blocking here
        let file = File::open(full_file_path).with_context(|| {
            format!(
                "failed to open file {:?} for label {:?}",
                full_file_path, root_relative_path
            )
        })?;

        let before_metadata = file.metadata()?;

        let mut reader = BufReader::with_capacity(HASH_BUFFER_SIZE, file);
        let mut hasher = digest::Context::new(&SHA256);

        loop {
            let buf_len = {
                let buffer = reader.fill_buf()?;
                if buffer.len() == 0 {
                    break;
                }
                hasher.update(buffer);
                buffer.len()
            };
            reader.consume(buf_len);
        }

        let after_metadata = reader.get_mut().metadata()?;

        // Only accept the hash if the file mtime and node did not change during read
        // TODO: This uses the file handle, does it make sense to care about the path? Probably this will be determined by how it interacts with file watching.
        if before_metadata.modified()? != after_metadata.modified()?
            || before_metadata.file_node_identifier()? != after_metadata.file_node_identifier()?
        {
            continue;
        }

        cfg_if::cfg_if! {
            if #[cfg(unix)] {
                use std::os::unix::fs::PermissionsExt;
                let executable = before_metadata.permissions().mode() & 0o100 != 0;
            } else {
                let executable = false;
            }
        }

        return Ok(FileStatus {
            mtime: after_metadata.modified()?,
            file_node_identifier: after_metadata.file_node_identifier()?,
            hash: hasher.finish().into(),
            executable,
        });
    }

    bail!("source file was modified too many times while trying to observe it");
}

impl fmt::Debug for SourceEntryMonitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceEntryMonitor")
            .field("full_file_path", &self.full_file_path)
            .field("root_relative_path", &self.root_relative_path)
            .finish()
    }
}
