use std::{path::Path, sync::Arc, task::Waker, time::Instant};

use anyhow::Result;
use cealn_data::{file_entry::FileType, Label};
use compio_fs::Directory;
use futures::prelude::*;

use crate::entry::{SourceEntryMonitor, Status};

#[derive(Clone, Debug)]
pub struct SourceReference {
    monitor: Arc<SourceEntryMonitor>,

    /// The status observed prior to allowing the build to observe the file in question
    ///
    /// The reference is only considered stable if this remains constant for the duration of the observation.
    pre_observation_status: Status,
}

impl SourceReference {
    pub(crate) async fn new(monitor: Arc<SourceEntryMonitor>) -> Result<Self> {
        let pre_observation_status = monitor.current_status().await?;

        Ok(SourceReference {
            monitor,

            pre_observation_status,
        })
    }

    pub async fn has_changed(&self) -> Result<bool> {
        let current_status = self.monitor.current_status().await?;
        if !self.pre_observation_status.equivalent(&current_status) {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub async fn has_changed_until(&self, instant: Instant) -> Result<bool> {
        let current_status = self.monitor.status_until(instant).await?;
        if !self.pre_observation_status.equivalent(&current_status) {
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub fn pre_observation_status(&self) -> &Status {
        &self.pre_observation_status
    }

    #[inline]
    pub fn file_type(&self) -> Option<FileType> {
        match &self.pre_observation_status {
            Status::Directory(_) => Some(FileType::Directory),
            Status::File(_) => Some(FileType::Regular),
            Status::Symlink(_) => Some(FileType::Symlink),
            Status::NotFound => None,
        }
    }

    #[inline]
    pub fn full_file_path(&self) -> &Path {
        self.monitor.full_file_path()
    }

    #[inline]
    pub fn root_relative_path(&self) -> &Label {
        self.monitor.root_relative_path()
    }

    pub async fn reference_child(&self, name: &Label) -> Result<SourceReference> {
        // TODO: avoid double-stat here when file is new?
        let monitor = self.monitor.monitor_child(name).await?;
        SourceReference::new(monitor).await
    }

    pub async fn reference_children(&self) -> Result<Vec<SourceReference>> {
        let mut directory = Directory::open(self.full_file_path()).await?;
        let entries: Vec<_> = directory.read_dir().await?.try_collect().await?;
        let mut source_references = Vec::new();
        for entry in entries {
            let file_name = entry.file_name();
            let Some(subpath) = file_name.to_str() else {
                continue;
            };
            let reference = self.reference_child(Label::new(subpath).unwrap()).await?;
            source_references.push(reference);
        }
        // Sort for determinisim
        source_references.sort_by(|a, b| {
            a.full_file_path()
                .file_name()
                .unwrap()
                .cmp(b.full_file_path().file_name().unwrap())
        });
        Ok(source_references)
    }

    /// Indicates whether this file existed when this reference was created
    #[inline]
    pub fn existed(&self) -> bool {
        match self.pre_observation_status {
            Status::NotFound => false,
            _ => true,
        }
    }

    #[inline]
    pub fn wake_on_changed<F>(&self, factory: F) -> bool
    where
        F: FnOnce() -> Waker,
    {
        self.monitor.wake_on_changed(factory)
    }
}
