use std::process::Command;

use anyhow::{bail, Context as _, Result};
use cealn_cli_support::console::{ComposeEvent, ComposeEventData, ComposeEventSource};
use cealn_rules_compose_data::Volume;
use futures::prelude::*;
use k8s_openapi::api::core::v1::Pod;
use kube_client::Api;

use super::RunContext;

impl RunContext {
    pub(crate) async fn push_volumes(&self) -> Result<()> {
        let manifest = self.base.manifest.as_ref().unwrap();
        futures::stream::iter(manifest.volumes.iter())
            .map(|volume| self.push_volume(volume))
            .buffer_unordered(16)
            .try_collect()
            .await
    }

    async fn push_volume(&self, volume: &Volume) -> Result<()> {
        let event_source = ComposeEventSource::VolumeSync {
            namespace: volume.namespace.clone(),
            persistent_volume_claim: volume.persistent_volume_claim.clone(),
        };
        self.base.push_compose_event(ComposeEvent {
            source: Some(event_source.clone()),
            data: ComposeEventData::Start,
        });

        let mut source_dir = self.base.compose_path.join("volumes");
        source_dir.push(&volume.namespace);
        source_dir.push(&volume.persistent_volume_claim);
        // Ensure trailing slash for rsync
        source_dir.push("");

        let pod_api: Api<Pod> = Api::namespaced(self.base.kube_client.clone(), &volume.namespace);
        let pod = pod_api.get(&volume.sync_pod).await.with_context(|| {
            format!(
                "failed to find sync pod {:?} in namespace {:?}",
                volume.sync_pod, volume.namespace
            )
        })?;
        // TODO: wait for pod to get IP if missing
        let pod_ip = pod
            .status
            .as_ref()
            .and_then(|status| status.pod_ip.as_deref())
            .with_context(|| {
                format!(
                    "sync pod {:?} in namespace {:?} has no IP address",
                    volume.sync_pod, volume.namespace
                )
            })?;

        // FIXME: don't hard code this
        let domain_name = format!(
            "{ip}.{namespace}.pod.kind-john.hardscience.test",
            ip = pod_ip.replace('.', "-"),
            namespace = volume.namespace
        );

        let status = tokio::task::spawn_blocking({
            let sync_pod_module = volume.sync_pod_module.clone();
            move || {
                Command::new("rsync")
                    .args(&["--recursive", "--links", "--delete", "--delete-after"])
                    .arg(&source_dir)
                    .arg(format!("rsync://{}/{}", domain_name, sync_pod_module))
                    .status()
            }
        })
        .await??;

        if !status.success() {
            bail!("rsync failed with exit code {}", status);
        }

        self.base.push_compose_event(ComposeEvent {
            source: Some(event_source.clone()),
            data: ComposeEventData::End,
        });

        Ok(())
    }
}
