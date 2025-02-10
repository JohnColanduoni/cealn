use std::sync::Arc;

use cealn_data::Label;
use cealn_event::BuildEventData;
use futures::prelude::*;

use cealn_protocol::{
    event::BuildEventSource,
    query::{AllWorkspacesLoadQuery, LoadQuery, LoadQueryProduct, RootWorkspaceLoadQuery},
};
use tracing::info_span;

use crate::{
    graph::{
        graph::{GraphQuery, GraphQueryInternal, _Graph},
        node::_QueryRequest,
    },
    package,
};

pub struct LoadQueryRequest(pub(in crate::graph) _QueryRequest<LoadQuery>);

impl GraphQuery for LoadQuery {
    type Request = LoadQueryRequest;
}

impl GraphQueryInternal for LoadQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<LoadQueryProduct>> + 'a;

    fn get_query_node(graph: &_Graph, query: LoadQuery) -> Arc<super::QueryNode<Self>> {
        graph.get_or_create_query(query, &graph.load_queries)
    }

    fn run<'a>(runner: &'a mut super::QueryRunnerDriver<Self>) -> Self::Run<'a> {
        async move {
            // Perform canonicalization if necessary
            let shared = runner.shared.clone();
            if let Some(canonical_package_name) = runner.canonicalize_label(&shared.query.package).await? {
                // Redirect to canonical query
                let canonical_result = runner
                    .query(LoadQuery {
                        package: canonical_package_name,
                    })
                    .await;
                return Ok(canonical_result.output_ref()?.clone());
            }

            let workspaces = runner.query(AllWorkspacesLoadQuery {}).await;
            let workspaces = workspaces.output_ref()?;

            let named_workspace_fs = runner.build_workspaces_fs(workspaces).await?;
            let python_interpreter = runner.get_python_interpreter().await?;

            runner.events().send(BuildEventData::QueryRunStart);
            runner.sent_start_event = true;

            // FIXME: this can block, figure out appropriate scheduling or switch WASM to async mode
            let loader = package::Loader::new(
                &python_interpreter,
                runner.events().fork(),
                named_workspace_fs.to_handle(),
                runner.shared.query.package.clone(),
            )
            .await?;
            let product = loader.load(&runner.graph.executor, runner.events_live.fork()).await?;

            Ok(product)
        }
    }

    fn as_event_source(&self) -> BuildEventSource {
        BuildEventSource::PackageLoad {
            label: self.package.clone(),
        }
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!("query", kind = "load", package = %self.package)
    }
}
