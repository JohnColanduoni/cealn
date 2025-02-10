use std::sync::Arc;

use anyhow::Context as _;
use cealn_data::{
    file_entry::{FileEntryRef, FileType},
    rule::{BuildConfig, Target},
    LabelBuf,
};
use cealn_protocol::{
    event::BuildEventSource,
    query::{AnalysisQuery, LoadQuery, OutputQuery, OutputQueryProduct, QueryType},
};
use futures::prelude::*;
use serde::Serialize;
use tracing::info_span;

use crate::graph::{
    graph::{GraphQuery, GraphQueryInternal, _Graph},
    node::{QueryNode, QueryRunnerDriver, _QueryRequest},
};

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Debug)]
pub struct TargetQuery {
    pub label: LabelBuf,
    pub build_config: BuildConfig,
}

#[derive(Clone, PartialEq, Eq, Serialize, Debug)]
pub struct TargetQueryProduct {
    pub target: Target,
}

impl QueryType for TargetQuery {
    type Product = TargetQueryProduct;

    const KIND: &'static str = "target";
}

pub struct TargetQueryRequest(pub(in crate::graph) _QueryRequest<TargetQuery>);

impl GraphQuery for TargetQuery {
    type Request = TargetQueryRequest;
}

impl GraphQueryInternal for TargetQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<TargetQueryProduct>> + Send + 'a;

    fn get_query_node(graph: &_Graph, query: Self) -> Arc<QueryNode<Self>> {
        graph.get_or_create_query(query, &graph.target_queries)
    }

    fn run<'a>(runner: &'a mut QueryRunnerDriver<Self>) -> Self::Run<'a> {
        async move {
            let target_label_parts = runner.shared.query.label.parts();

            let package_label = target_label_parts.full_package().to_owned();
            let target_name = target_label_parts
                .target
                .with_context(|| {
                    format!(
                        "absolute target label expected, but got {:?}",
                        runner.shared.query.label
                    )
                })?
                .to_owned();

            let package;
            let parent_analysis;
            let target = match target_name.parent() {
                Some(parent_target_name) => {
                    let parent_target = target_label_parts
                        .full_package()
                        .join_action(parent_target_name.as_str())
                        .unwrap();
                    parent_analysis = runner
                        .query(AnalysisQuery {
                            target_label: parent_target.clone(),
                            build_config: runner.shared.query.build_config.clone(),
                        })
                        .await;
                    let parent_analysis = parent_analysis.output_ref()?;

                    let last_target_name = target_name.file_name().unwrap().as_str();
                    parent_analysis
                        .analysis
                        .synthetic_targets
                        .iter()
                        .find(|target| target.name == last_target_name)
                        .with_context(|| {
                            format!(
                                "no such synthetic target {:?} in target {:?} with build config {:?}",
                                last_target_name, parent_target, &runner.shared.query.build_config,
                            )
                        })?
                }
                None => {
                    package = runner
                        .query(LoadQuery {
                            package: package_label.to_owned(),
                        })
                        .await;
                    let package = package.output_ref()?;

                    package
                        .package
                        .targets
                        .iter()
                        .find(|target| target.name == target_name.as_str())
                        .with_context(|| format!("no such target {:?} in package {:?}", target_name, package_label))?
                }
            };

            Ok(TargetQueryProduct { target: target.clone() })
        }
    }

    fn as_event_source(&self) -> BuildEventSource {
        BuildEventSource::InternalQuery
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!("query", kind = "target")
    }
}
