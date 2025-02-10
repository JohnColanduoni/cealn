use std::{
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    task::Waker,
    thread::JoinHandle,
};

use anyhow::Result;
use ignore::gitignore::Gitignore;
use parking_lot::Mutex;
use tracing::error;

use cealn_data::{label::Segment, Label};

use crate::{watcher::WatchPort, SourceReference};

use super::entry::SourceEntryMonitor;

pub struct SourceMonitor(Arc<_SourceMonitor>);

struct _SourceMonitor {
    canonical_workspace_root: PathBuf,

    root: Arc<SourceEntryMonitor>,

    watch_port_thread: JoinHandle<()>,
}

pub(crate) struct Shared {
    pub ignore: Gitignore,
    pub watch_port: Option<WatchPort>,
}

pub struct AnyChangeObserveGuard {
    imp: Option<crate::watcher::AnyChangeObserveGuard>,
}

impl SourceMonitor {
    pub async fn new(canonical_workspace_root: PathBuf) -> Result<SourceMonitor> {
        // Prepare ignore patterns
        let (ignore, ignore_error) = Gitignore::new(&canonical_workspace_root.join(".cealnignore"));
        if let Some(error) = ignore_error {
            error!("error parsing .cealnignore file: {}", error);
        }

        let watch_port = WatchPort::new()?;
        let shared = Arc::new(Shared {
            ignore,
            watch_port: Some(watch_port.clone()),
        });

        let watch_port_thread = std::thread::Builder::new()
            .name("cealn-watch".to_owned())
            .spawn(move || loop {
                watch_port.poll(None).unwrap();
            })?;

        let root = SourceEntryMonitor::new_root(canonical_workspace_root.clone(), shared.clone())?;

        Ok(SourceMonitor(Arc::new(_SourceMonitor {
            canonical_workspace_root,
            root,
            watch_port_thread,
        })))
    }

    pub fn canonical_workspace_root(&self) -> &Path {
        &self.0.canonical_workspace_root
    }

    pub async fn reference(&self, label: &Label) -> Result<SourceReference> {
        let monitor = self.monitor(label).await?;
        Ok(SourceReference::new(monitor).await?)
    }

    pub fn will_observe_any_change(&self) -> AnyChangeObserveGuard {
        let Some(watch_port) = self.0.root.shared.watch_port.as_ref() else {
            return AnyChangeObserveGuard { imp: None };
        };
        let imp = watch_port.will_observe_any_change();
        AnyChangeObserveGuard { imp: Some(imp) }
    }

    pub async fn wait_for_any_change(&self, guard: AnyChangeObserveGuard) -> Result<()> {
        let Some(watch_port) = self.0.root.shared.watch_port.as_ref() else {
            return Ok(());
        };
        let guard = guard.imp.unwrap();

        watch_port.wait_for_any_change(guard).await
    }

    pub fn any_change_guard_check_dirty(&self, guard: &mut AnyChangeObserveGuard) -> Result<bool> {
        let Some(watch_port) = self.0.root.shared.watch_port.as_ref() else {
            return Ok(false);
        };
        let guard = guard.imp.as_mut().unwrap();

        watch_port.any_change_guard_check_dirty(guard)
    }

    async fn monitor(&self, label: &Label) -> Result<Arc<SourceEntryMonitor>> {
        // We panic here because this is always an internal error, checking this should be handled at a higher
        // level
        assert!(label.is_workspace_relative(), "expected workspace relative label");

        let mut current_entry = self.0.root.clone();

        for segment in label.segments() {
            match segment {
                Segment::CurrentDirectory | Segment::ParentDirectory | Segment::All => {
                    // We panic here because this is always an internal error, checking this should be handled at a higher
                    // level
                    panic!("expected normalized label")
                }
                Segment::Filename(name) => {
                    current_entry = current_entry.monitor_child(name).await?;
                }
                Segment::Colon => {}
            }
        }

        Ok(current_entry)
    }
}

impl fmt::Debug for SourceMonitor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SourceMonitor")
            .field("canonical_root", &self.0.canonical_workspace_root)
            .finish()
    }
}
