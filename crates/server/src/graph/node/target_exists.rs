use std::sync::Arc;

use anyhow::Context as _;
use cealn_data::{
    file_entry::{FileEntryRef, FileType},
    rule::BuildConfig,
    LabelBuf,
};
use cealn_protocol::{
    event::BuildEventSource,
    query::{LoadQuery, OutputQuery, OutputQueryProduct, QueryType},
};
use futures::prelude::*;
use serde::Serialize;
use tracing::info_span;

use crate::graph::{
    graph::{GraphQuery, GraphQueryInternal, _Graph},
    node::{QueryNode, QueryRunnerDriver, _QueryRequest},
};

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Debug)]
pub struct TargetExistsQuery {
    pub label: LabelBuf,
    pub build_config: BuildConfig,
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Debug)]
pub struct TargetExistsQueryProduct {
    pub exists: bool,
}

impl QueryType for TargetExistsQuery {
    type Product = TargetExistsQueryProduct;

    const KIND: &'static str = "target-exists";
}

pub struct FileTypeQueryRequest(pub(in crate::graph) _QueryRequest<TargetExistsQuery>);

impl GraphQuery for TargetExistsQuery {
    type Request = FileTypeQueryRequest;
}

impl GraphQueryInternal for TargetExistsQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<TargetExistsQueryProduct>> + Send + 'a;

    fn get_query_node(graph: &_Graph, query: Self) -> Arc<QueryNode<Self>> {
        graph.get_or_create_query(query, &graph.target_exists_queries)
    }

    fn run<'a>(runner: &'a mut QueryRunnerDriver<Self>) -> Self::Run<'a> {
        async move {
            let parts = runner.shared.query.label.parts();
            let target = parts.target.context("not a target label")?;

            let package_result = runner
                .query_speculatve(LoadQuery {
                    package: parts.full_package().to_owned(),
                })
                .await;
            let Ok(package) = package_result.output().as_ref() else {
                // FIXME: distinguish package not existing from other errors
                return Ok(TargetExistsQueryProduct { exists: false });
            };
            if target.segments().count() > 1 {
                todo!()
            }
            let target_name = target.as_str();
            Ok(TargetExistsQueryProduct {
                exists: package.package.targets.iter().any(|target| target.name == target_name),
            })
        }
    }

    fn as_event_source(&self) -> BuildEventSource {
        BuildEventSource::InternalQuery
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!("query", kind = "target-exists")
    }
}
