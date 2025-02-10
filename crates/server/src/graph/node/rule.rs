use std::{collections::BTreeMap, sync::Arc};

use anyhow::{anyhow, Context as _};
use cealn_depset::{ConcreteFiletree, DepMap};
use cealn_event::BuildEventData;
use futures::{future::BoxFuture, prelude::*};

use cealn_data::{
    action::{ActionData, ActionOutput, LabelAction},
    depmap::{ConcreteDepmapReference, DepmapHash},
    file_entry::{FileEntry, FileEntryRef, FileHash, FileHashRef, FileType},
    reference::Reference,
    rule::{BuildConfig, Provider},
    workspace::GlobalDefaultProvider,
    Label, LabelBuf,
};
use cealn_protocol::{
    event::BuildEventSource,
    query::{
        AllWorkspacesLoadQuery, AnalysisQuery, AnalysisQueryProduct, LoadQuery, OutputQuery, RootWorkspaceLoadQuery,
    },
};
use tracing::info_span;

use crate::{
    graph::{
        graph::{GraphQuery, GraphQueryInternal, _Graph},
        node::{
            QueryRunnerDriver, _QueryRequest, action::ActionQuery, file_type::FileTypeQuery, target::TargetQuery,
            target_exists::TargetExistsQuery,
        },
        QueryResult,
    },
    rule::{self, LabeledFileContents},
};

pub struct AnalysisQueryRequest(pub(in crate::graph) _QueryRequest<AnalysisQuery>);

impl GraphQuery for AnalysisQuery {
    type Request = AnalysisQueryRequest;
}

impl GraphQueryInternal for AnalysisQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<AnalysisQueryProduct>> + 'a;

    fn get_query_node(graph: &_Graph, query: AnalysisQuery) -> Arc<super::QueryNode<Self>> {
        graph.get_or_create_query(query, &graph.analysis_queries)
    }

    fn run<'a>(runner: &'a mut super::QueryRunnerDriver<Self>) -> Self::Run<'a> {
        async move {
            // Perform canonicalization if necessary
            let shared = runner.shared.clone();
            if let Some(canonical_target_label) = runner.canonicalize_label(&shared.query.target_label).await? {
                // Redirect to canonical query
                let canonical_result = runner
                    .query(AnalysisQuery {
                        target_label: canonical_target_label,
                        build_config: shared.query.build_config.clone(),
                    })
                    .await;
                return Ok(canonical_result.output_ref()?.clone());
            }

            let target_label_parts = runner.shared.query.target_label.parts();

            let target_result = runner
                .query(TargetQuery {
                    label: target_label_parts.full_target().to_owned(),
                    build_config: runner.shared.query.build_config.clone(),
                })
                .await;
            let target = &target_result.output_ref()?.target;

            let workspaces = runner.query(AllWorkspacesLoadQuery {}).await;
            let workspaces = workspaces.output_ref()?;
            let named_workspace_fs = runner.build_workspaces_fs(workspaces).await?;
            let python_interpreter = runner.get_python_interpreter().await?;

            runner.events().send(BuildEventData::QueryRunStart);
            runner.sent_start_event = true;

            // FIXME: this can block, figure out appropriate scheduling or switch WASM to async mode
            let loader = rule::Analyzer::new(
                &python_interpreter,
                runner.events().fork(),
                named_workspace_fs.to_handle(),
                runner.shared.query.target_label.clone(),
                target.clone(),
                runner.shared.query.build_config.clone(),
            )
            .await?;
            let context = AnalysisContext { runner };
            let product = loader.analyze(&runner.graph.executor, &context).await?;

            Ok(product)
        }
    }

    fn as_event_source(&self) -> BuildEventSource {
        BuildEventSource::RuleAnalysis {
            target_label: self.target_label.clone(),
        }
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!("query", kind = "analysis", target = %self.target_label)
    }
}

struct AnalysisContext<'a> {
    runner: &'a QueryRunnerDriver<AnalysisQuery>,
}

impl rule::Context for AnalysisContext<'_> {
    fn labeled_target_exists<'a>(&'a self, label: &'a Label) -> BoxFuture<'a, anyhow::Result<bool>> {
        async move {
            let result = self
                .runner
                .query(TargetExistsQuery {
                    label: label.to_owned(),
                    build_config: self.runner.shared.query.build_config.clone(),
                })
                .await;
            let product = result.output_ref()?;
            Ok(product.exists)
        }
        .boxed()
    }

    fn labeled_file_exists<'a>(&'a self, label: &'a Label) -> BoxFuture<'a, anyhow::Result<bool>> {
        async move {
            let result = self
                .runner
                .query(FileTypeQuery {
                    label: label.to_owned(),
                    build_config: self.runner.shared.query.build_config.clone(),
                })
                .await;
            let product = result.output_ref()?;
            Ok(product.file_type.is_some())
        }
        .boxed()
    }

    fn labeled_file_is_file<'a>(&'a self, label: &'a Label) -> BoxFuture<'a, anyhow::Result<bool>> {
        async move {
            let result = self
                .runner
                .query(FileTypeQuery {
                    label: label.to_owned(),
                    build_config: self.runner.shared.query.build_config.clone(),
                })
                .await;
            let product = result.output_ref()?;
            Ok(product.file_type == Some(FileType::Regular))
        }
        .boxed()
    }

    fn load_providers<'a>(
        &'a self,
        provider_target: &'a Label,
        build_config: BuildConfig,
    ) -> BoxFuture<'a, anyhow::Result<Vec<Provider>>> {
        async move {
            let provider_result = self
                .runner
                .query(AnalysisQuery {
                    target_label: provider_target.to_owned(),
                    build_config,
                })
                .await;
            let provider_result = provider_result.output_ref()?;

            Ok(provider_result.analysis.providers.clone())
        }
        .boxed()
    }

    fn load_file_label<'a>(
        &'a self,
        label: &'a Label,
    ) -> BoxFuture<'a, anyhow::Result<Option<ConcreteDepmapReference>>> {
        async move {
            let parts = label.parts();

            if let Some(action_id) = parts.action_id {
                let output_result = self
                    .runner
                    .query(OutputQuery {
                        target_label: label.to_owned(),
                        build_config: self.runner.shared.query.build_config.clone(),
                    })
                    .await;
                let output_result = output_result.output_ref()?;

                Ok(output_result.reference.clone())
            } else if let Some(source_file) = self.runner.reference_source_file_as_depmap(label).await? {
                Ok(Some(source_file))
            } else {
                Ok(None)
            }
        }
        .boxed()
    }

    fn load_global_provider<'a>(
        &'a self,
        provider_ref: &'a Reference,
        build_config: BuildConfig,
    ) -> BoxFuture<'a, anyhow::Result<Option<Provider>>> {
        async move {
            let workspace_result = self.runner.query(RootWorkspaceLoadQuery).await;
            let workspace_result = workspace_result.output_ref()?;
            let mut provider_target = None;
            for supplied_provider in &workspace_result.global_default_providers {
                match supplied_provider {
                    GlobalDefaultProvider::Static {
                        provider_type,
                        providing_target,
                    } => {
                        // TODO: any canonicalization?
                        if provider_type == provider_ref {
                            provider_target = Some(providing_target.to_owned());
                            break;
                        }
                    }
                }
            }

            let provider_target = if let Some(label) = provider_target {
                label
            } else {
                return Ok(None);
            };

            let provider_result = self
                .runner
                .query(AnalysisQuery {
                    target_label: provider_target.clone(),
                    build_config,
                })
                .await;
            let provider_result = provider_result.output_ref()?;

            let provider = provider_result
                .analysis
                .providers
                .iter()
                .find(|provider| &provider.reference == provider_ref)
                .ok_or_else(|| {
                    anyhow!(
                        "target {} did not supply provider {}",
                        provider_target,
                        provider_ref.qualname
                    )
                })?;

            Ok(Some(provider.clone()))
        }
        .boxed()
    }

    fn run_action<'a>(
        &'a self,
        action: cealn_data::action::LabelAction,
        partial_actions: BTreeMap<LabelBuf, LabelAction>,
    ) -> BoxFuture<'a, anyhow::Result<ActionOutput>> {
        async move {
            let action_result = self
                .runner
                .query(ActionQuery {
                    partial_actions,
                    build_config: match &action.data {
                        ActionData::Run(_) => self.runner.shared.query.build_config.transition_to_host(),
                        _ => self.runner.shared.query.build_config.clone(),
                    },
                    action,
                })
                .await;
            let action_result = action_result.output_ref()?;
            Ok(action_result.clone())
        }
        .boxed()
    }

    fn get_filetree<'a>(&'a self, reference: &'a DepmapHash) -> BoxFuture<'a, anyhow::Result<ConcreteFiletree>> {
        async move {
            self.runner
                .graph
                .lookup_filetree_cache(reference)
                .await?
                .ok_or_else(|| anyhow!("received invalid filetree reference from analysis"))
        }
        .boxed()
    }

    fn open_cache_file<'a>(
        &'a self,
        digest: FileHashRef<'a>,
        executable: bool,
    ) -> BoxFuture<'a, anyhow::Result<Arc<dyn cealn_runtime::api::Handle>>> {
        async move {
            let file_guard = self.runner.reference_cache_file(digest, executable).await?;
            crate::runtime::cache::open(file_guard).await
        }
        .boxed()
    }
}

impl Future for AnalysisQueryRequest {
    type Output = QueryResult<AnalysisQueryProduct>;

    #[inline]
    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        self.0.poll_unpin(cx)
    }
}
