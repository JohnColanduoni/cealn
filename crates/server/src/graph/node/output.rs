use std::{collections::BTreeMap, io, path::PathBuf, sync::Arc};

use anyhow::{anyhow, Context as AnyhowContext};
use cealn_data::{
    depmap::ConcreteDepmapReference,
    label::{self, LabelPath},
};
use cealn_depset::ConcreteFiletree;
use cealn_event::BuildEventData;
use futures::prelude::*;

use cealn_protocol::{
    event::BuildEventSource,
    query::{AnalysisQuery, OutputQuery, OutputQueryProduct},
};
use tracing::info_span;

use crate::graph::{
    graph::{GraphQuery, GraphQueryInternal, _Graph},
    node::{_QueryRequest, action::ActionQuery},
    QueryResult,
};

pub struct OutputQueryRequest(pub(in crate::graph) _QueryRequest<OutputQuery>);

impl Unpin for OutputQueryRequest {}

impl GraphQuery for OutputQuery {
    type Request = OutputQueryRequest;
}

impl GraphQueryInternal for OutputQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<OutputQueryProduct>> + 'a;

    fn get_query_node(graph: &_Graph, query: OutputQuery) -> Arc<super::QueryNode<Self>> {
        graph.get_or_create_query(query, &graph.output_queries)
    }

    fn run<'a>(runner: &'a mut super::QueryRunnerDriver<Self>) -> Self::Run<'a> {
        async move {
            runner.events().send(BuildEventData::QueryRunStart);
            runner.sent_start_event = true;

            // Perform canonicalization if necessary
            let shared = runner.shared.clone();
            if let Some(canonical_target_label) = runner.canonicalize_label(&shared.query.target_label).await? {
                // Redirect to canonical query
                let canonical_result = runner
                    .query(OutputQuery {
                        target_label: canonical_target_label,
                        build_config: shared.query.build_config.clone(),
                    })
                    .await;
                return Ok(canonical_result.output_ref()?.clone());
            }

            let parts = runner.shared.query.target_label.parts();

            if parts.target.is_none() {
                // Source file
                let reference = runner
                    .reference_source_file_as_depmap(&runner.shared.query.target_label)
                    .await?
                    .with_context(|| format!("no such file at label {:?}", runner.shared.query.target_label))?;
                return Ok(OutputQueryProduct {
                    reference: Some(reference),
                });
            }

            let analysis = runner
                .query(AnalysisQuery {
                    target_label: parts.full_target().to_owned(),
                    build_config: runner.shared.query.build_config.clone(),
                })
                .await;
            let analysis = analysis.output_ref()?;

            if let Some(action_id) = parts.action_id {
                let action = analysis
                    .analysis
                    .actions
                    .iter()
                    .find(|x| x.id == action_id)
                    .ok_or_else(|| anyhow!("couldn't find action with id {} in {}", action_id, parts.full_target()))?;
                let action_result = runner
                    .query(ActionQuery {
                        action: action.clone(),
                        partial_actions: Default::default(),
                        build_config: runner.shared.query.build_config.clone(),
                    })
                    .await;
                let action_output = action_result.output_ref()?;

                let action_path = match parts.action_path {
                    Some(action_path) => Some(
                        action_path
                            .normalize_require_descending()
                            .context("action path escapes root")?
                            .into_owned(),
                    ),
                    None => None,
                };

                Ok(OutputQueryProduct {
                    reference: Some(ConcreteDepmapReference {
                        hash: action_output.files.clone(),
                        subpath: action_path,
                    }),
                })
            } else {
                // Build mounts
                for (dest_subpath, src_subpath) in &analysis.output_mounts {
                    let (action_id, action_path) = match src_subpath.split_once('/') {
                        Some((action_id, action_path)) => (action_id, Some(action_path)),
                        None => (&**src_subpath, None),
                    };

                    let mut dest_realpath: PathBuf = match parts.root {
                        label::Root::WorkspaceRelative => todo!(),
                        label::Root::PackageRelative => todo!(),
                        // FIXME: hack
                        label::Root::Workspace(w) if w == "io.hardscience" => {
                            runner.graph.source_view.canonical_workspace_root().to_owned()
                        }
                        label::Root::Workspace(_) => todo!(),
                    };
                    if let Some(package) = parts.package {
                        dest_realpath.push(package.to_native_relative_path());
                    }
                    dest_realpath.push(dest_subpath);

                    let action = analysis
                        .analysis
                        .actions
                        .iter()
                        .find(|action| &action.id == action_id)
                        .with_context(|| format!("missing action with id {}", action_id))?;

                    let action_result = runner
                        .query(ActionQuery {
                            action: action.clone(),
                            partial_actions: Default::default(),
                            build_config: runner.shared.query.build_config.clone(),
                        })
                        .await;
                    let action_output = action_result.output_ref()?;

                    let output_files = runner
                        .graph
                        .lookup_filetree_cache(&action_output.files)
                        .await?
                        .context("failed to find built depmap")?;
                    let output_files = if let Some(action_path) = action_path {
                        ConcreteFiletree::builder()
                            .merge_filtered(
                                LabelPath::new("")
                                    .unwrap()
                                    .normalize_require_descending()
                                    .unwrap()
                                    .as_ref(),
                                LabelPath::new(action_path)?
                                    .normalize_require_descending()
                                    .context("invalid action path")?
                                    .as_ref(),
                                &[".*"],
                                output_files,
                            )
                            .build()
                    } else {
                        output_files
                    };

                    match compio_fs::remove_dir_all(&dest_realpath).await {
                        Ok(()) => {}
                        Err(ref err) if err.kind() == io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err.into()),
                    }
                    cealn_fs_materialize::materialize_for_output(
                        &*runner.graph.cache_subsystem,
                        &dest_realpath,
                        output_files,
                    )
                    .await?;
                }

                Ok(OutputQueryProduct { reference: None })
            }
        }
    }

    fn as_event_source(&self) -> BuildEventSource {
        BuildEventSource::Output {
            label: self.target_label.clone(),
        }
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!("query", kind = "output", target_label = %self.target_label)
    }
}

impl Future for OutputQueryRequest {
    type Output = QueryResult<OutputQueryProduct>;

    #[inline]
    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        self.0.poll_unpin(cx)
    }
}
