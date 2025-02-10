use std::sync::Arc;

use anyhow::{bail, Context};
use cealn_event::BuildEventData;
use futures::{prelude::*, stream::FuturesOrdered, StreamExt};

use cealn_data::{
    label,
    workspace::{LocalWorkspaceParams, LocalWorkspaceResolved},
    Label,
};
use cealn_source_fs::SourceFs;

use cealn_protocol::{
    event::BuildEventSource,
    query::{
        AllWorkspacesLoadQuery, AllWorkspacesLoadQueryProduct, RootWorkspaceLoadQuery, RootWorkspaceLoadQueryProduct,
    },
};
use tracing::{info_span, Instrument};

use crate::{
    graph::{
        graph::{GraphQuery, GraphQueryInternal, _Graph},
        node::_QueryRequest,
        QueryResult,
    },
    workspace,
};

use super::QueryNode;

pub struct RootWorkspaceLoadQueryRequest(pub(in crate::graph) _QueryRequest<RootWorkspaceLoadQuery>);

pub struct AllWorkspacesLoadQueryRequest(pub(in crate::graph) _QueryRequest<AllWorkspacesLoadQuery>);

impl GraphQuery for RootWorkspaceLoadQuery {
    type Request = RootWorkspaceLoadQueryRequest;
}

impl GraphQueryInternal for RootWorkspaceLoadQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<RootWorkspaceLoadQueryProduct>> + 'a;

    fn get_query_node(graph: &_Graph, _query: RootWorkspaceLoadQuery) -> Arc<QueryNode<Self>> {
        graph.load_root_workspace.clone()
    }

    fn run<'a>(runner: &'a mut super::QueryRunnerDriver<Self>) -> Self::Run<'a> {
        async move {
            let workspace_fs = runner.get_sourcefs(Label::new("//").unwrap()).await?;
            let python_interpreter = runner.get_python_interpreter().await?;

            runner.events().send(BuildEventData::QueryRunStart);
            runner.sent_start_event = true;

            // FIXME: this can block, figure out appropriate scheduling or switch WASM to async mode
            let loader =
                workspace::Loader::new(&python_interpreter, runner.events().fork(), workspace_fs.to_handle()).await?;
            let product = loader.load(&runner.graph.executor).await?;

            Ok(product)
        }
    }

    fn as_event_source(&self) -> BuildEventSource {
        BuildEventSource::RootWorkspaceLoad
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!("query", kind = "root_workspace_load")
    }
}

impl GraphQuery for AllWorkspacesLoadQuery {
    type Request = AllWorkspacesLoadQueryRequest;
}

impl GraphQueryInternal for AllWorkspacesLoadQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<AllWorkspacesLoadQueryProduct>> + 'a;

    fn get_query_node(graph: &_Graph, _query: AllWorkspacesLoadQuery) -> Arc<QueryNode<Self>> {
        graph.load_all_workspaces.clone()
    }

    fn run<'a>(runner: &'a mut super::QueryRunnerDriver<Self>) -> Self::Run<'a> {
        let span = info_span!("all_workspaces_load_query");
        async move {
            let root_workspace = runner.query(RootWorkspaceLoadQuery {}).await;
            let root_workspace = root_workspace.output_ref()?;

            runner.events().send(BuildEventData::QueryRunStart);
            runner.sent_start_event = true;

            // Resolve local workspaces recursively
            let mut local_workspaces = Vec::with_capacity(root_workspace.local_workspaces.len());
            let mut resolving_local_workspaces = FuturesOrdered::new();
            for local_workspace in root_workspace.local_workspaces.iter() {
                let resolver = resolve_local_workspace(runner, local_workspace.clone()).await?;
                resolving_local_workspaces.push(resolver);
            }
            while let Some((local_workspace, product)) = resolving_local_workspaces.try_next().await? {
                // Recurse for any local workspaces
                for nested_local_workspace in product.local_workspaces {
                    let (nested_root, nested_path) = nested_local_workspace.path.split_root();
                    let root_path = match nested_root {
                        label::Root::WorkspaceRelative => local_workspace.path.join(nested_path).unwrap(),
                        label::Root::PackageRelative => {
                            // Treat package-relative as nested-workspace-root-relative
                            local_workspace.path.join(nested_path).unwrap()
                        }
                        label::Root::Workspace(_) => todo!(),
                    };
                    if root_path.split_package().1.is_some() {
                        bail!(
                            "found unexpected package separator in local workspace label {:?}",
                            nested_local_workspace.path
                        );
                    }
                    let resolver = resolve_local_workspace(runner, LocalWorkspaceParams { path: root_path }).await?;
                    resolving_local_workspaces.push(resolver);
                }

                // Emit resolved entry for this local workspace
                local_workspaces.push(LocalWorkspaceResolved {
                    name: product.name,
                    path: local_workspace.path,
                });
            }

            // Sort local workspaces to ensure determinism
            local_workspaces.sort_by(|a, b| a.path.cmp(&b.path));

            Ok(AllWorkspacesLoadQueryProduct {
                name: root_workspace.name.clone(),
                local_workspaces,
            })
        }
        .instrument(span)
    }

    fn as_event_source(&self) -> BuildEventSource {
        // FIXME: wrong
        BuildEventSource::RootWorkspaceLoad
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!("query", kind = "all_workspace_load")
    }
}

async fn resolve_local_workspace<'a>(
    runner: &'a mut super::QueryRunnerDriver<AllWorkspacesLoadQuery>,
    local_workspace: LocalWorkspaceParams,
) -> anyhow::Result<impl Future<Output = anyhow::Result<(LocalWorkspaceParams, RootWorkspaceLoadQueryProduct)>>> {
    let workspace_fs = runner.get_sourcefs(&local_workspace.path).await?;
    let python_interpreter = runner.get_python_interpreter().await?;
    let events = runner.events().fork();

    Ok(runner.graph.executor.spawn_immediate({
        let executor = runner.graph.executor.clone();
        async move {
            // FIXME: this can block, figure out appropriate scheduling or switch WASM to async mode
            let loader = workspace::Loader::new(&python_interpreter, events, workspace_fs.to_handle()).await?;
            let product = loader.load(&executor).await?;
            Ok((local_workspace, product))
        }
    }))
}

impl Future for RootWorkspaceLoadQueryRequest {
    type Output = QueryResult<RootWorkspaceLoadQueryProduct>;

    #[inline]
    fn poll(mut self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> std::task::Poll<Self::Output> {
        self.0.poll_unpin(cx)
    }
}
