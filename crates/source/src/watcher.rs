use std::{path::Path, sync::Arc, time::Duration};

use anyhow::Result;

use crate::{entry::SourceEntryMonitor, platform};

#[derive(Clone)]
pub(crate) struct WatchPort {
    imp: platform::WatchPort,
}

/// A filesystem node being watched
///
/// Note that this API does not attempt to paper over platform differences in recrusive watch capabilities, and instead
/// reports them so the higher level API can make watch decisions. In particular, on Linux a [`WatchNode`] will be
/// needed for each directory containing a file we wish to watch (and all parents), while on Windows and macOS we can
/// watch an entire directory tree.
pub(crate) struct WatchNode {
    imp: platform::WatchNode,
}

pub(crate) struct ObserveGuard {
    imp: platform::ObserveGuard,
}

pub(crate) struct AnyChangeObserveGuard {
    imp: platform::AnyChangeObserveGuard,
}

const WATCH_DEPTH: WatchDepth = platform::WATCH_DEPTH;

#[derive(Clone, Copy, Debug)]
pub enum WatchDepth {
    One,
    Infinite,
}

impl WatchPort {
    pub(crate) fn new() -> Result<WatchPort> {
        let imp = platform::WatchPort::new()?;
        Ok(WatchPort { imp })
    }

    pub(crate) fn watch(
        &self,
        path: impl AsRef<Path>,
        source_monitor: &Arc<SourceEntryMonitor>,
    ) -> Result<Option<WatchNode>> {
        let node = self.imp.watch(path.as_ref(), source_monitor)?;
        Ok(node.map(|imp| WatchNode { imp }))
    }

    pub(crate) fn poll(&self, timeout: Option<Duration>) -> Result<usize> {
        self.imp.poll(timeout)
    }

    pub(crate) fn will_observe_any_change(&self) -> AnyChangeObserveGuard {
        let imp = self.imp.will_observe_any_change();
        AnyChangeObserveGuard { imp }
    }

    pub(crate) async fn wait_for_any_change(&self, guard: AnyChangeObserveGuard) -> Result<()> {
        self.imp.wait_for_any_change(guard.imp).await
    }

    pub fn any_change_guard_check_dirty(&self, guard: &mut AnyChangeObserveGuard) -> Result<bool> {
        self.imp.any_change_guard_check_dirty(&mut guard.imp)
    }
}

impl WatchNode {
    pub(crate) fn duplicate(&self) -> Self {
        let imp = self.imp.duplicate();
        WatchNode { imp }
    }

    pub(crate) fn has_changed(&self) -> bool {
        self.imp.has_changed()
    }

    pub(crate) fn will_observe(&self) -> ObserveGuard {
        ObserveGuard {
            imp: self.imp.will_observe(),
        }
    }
}

impl ObserveGuard {
    pub(crate) fn commit(self, node: &WatchNode) {
        self.imp.commit(&node.imp)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn create_watch_port() {
        let _port = WatchPort::new().unwrap();
    }

    #[test]
    fn watch_mutate_in_directory() {
        let temp_dir = TempDir::new().unwrap();
        let file = temp_dir.path().join("test.txt");
        fs::write(&file, "something").unwrap();

        let port = WatchPort::new().unwrap();
        let node = port.watch(temp_dir.path()).unwrap().unwrap();
        port.poll(Some(Duration::from_millis(0))).unwrap();
        assert!(!node.has_changed());
        fs::write(&file, "something else").unwrap();
        port.poll(Some(Duration::from_millis(10))).unwrap();
        assert!(node.has_changed());
    }

    #[test]
    fn watch_atomic_update_in_directory() {
        let temp_dir = TempDir::new().unwrap();
        let watch_dir = temp_dir.path().join("watch-dir");
        fs::create_dir(&watch_dir).unwrap();
        let file = watch_dir.join("test.txt");
        fs::write(&file, "something").unwrap();

        let port = WatchPort::new().unwrap();
        let node = port.watch(&watch_dir).unwrap().unwrap();
        port.poll(Some(Duration::from_millis(0))).unwrap();
        assert!(!node.has_changed());
        let temp_file = temp_dir.path().join("temp.txt");
        fs::write(&temp_file, "something else").unwrap();
        port.poll(Some(Duration::from_millis(0))).unwrap();
        assert!(!node.has_changed());
        fs::rename(&temp_file, &file).unwrap();
        port.poll(Some(Duration::from_millis(10))).unwrap();
        assert!(node.has_changed());
    }
}
