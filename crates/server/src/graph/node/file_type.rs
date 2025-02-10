use std::sync::Arc;

use anyhow::Context as _;
use cealn_data::{
    file_entry::{FileEntryRef, FileType},
    rule::BuildConfig,
    LabelBuf,
};
use cealn_protocol::{
    event::BuildEventSource,
    query::{OutputQuery, OutputQueryProduct, QueryType},
};
use futures::prelude::*;
use serde::Serialize;
use tracing::info_span;

use crate::graph::{
    graph::{GraphQuery, GraphQueryInternal, _Graph},
    node::{QueryNode, QueryRunnerDriver, _QueryRequest},
};

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Debug)]
pub struct FileTypeQuery {
    pub label: LabelBuf,
    pub build_config: BuildConfig,
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Debug)]
pub struct FileTypeQueryProduct {
    pub file_type: Option<FileType>,
}

impl QueryType for FileTypeQuery {
    type Product = FileTypeQueryProduct;

    const KIND: &'static str = "file-type";
}

pub struct FileTypeQueryRequest(pub(in crate::graph) _QueryRequest<FileTypeQuery>);

impl GraphQuery for FileTypeQuery {
    type Request = FileTypeQueryRequest;
}

impl GraphQueryInternal for FileTypeQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<FileTypeQueryProduct>> + Send + 'a;

    fn get_query_node(graph: &_Graph, query: Self) -> Arc<QueryNode<Self>> {
        graph.get_or_create_query(query, &graph.file_type_queries)
    }

    fn run<'a>(runner: &'a mut QueryRunnerDriver<Self>) -> Self::Run<'a> {
        async move {
            let shared = runner.shared.clone();
            if let Some(canonical_label) = runner.canonicalize_label(&shared.query.label).await? {
                // Redirect to canonical query
                let canonical_result = runner
                    .query(FileTypeQuery {
                        label: canonical_label,
                        build_config: shared.query.build_config.clone(),
                    })
                    .await;
                return Ok(canonical_result.output_ref()?.clone());
            }

            let parts = runner.shared.query.label.parts();

            if let Some(action_id) = parts.action_id {
                let result = runner
                    .query_speculatve(OutputQuery {
                        target_label: runner.shared.query.label.clone(),
                        build_config: runner.shared.query.build_config.clone(),
                    })
                    .await;
                match result.output() {
                    Ok(OutputQueryProduct {
                        reference: Some(reference),
                    }) => {
                        let depmap = runner
                            .graph
                            .lookup_filetree_cache(&reference.hash)
                            .await?
                            .context("missing concrete depmap")?;

                        let depmap_path = match &reference.subpath {
                            Some(subpath) => subpath,
                            None => {
                                return Ok(FileTypeQueryProduct {
                                    file_type: Some(FileType::Directory),
                                })
                            }
                        };

                        match depmap.get(depmap_path.as_ref())? {
                            Some(FileEntryRef::Regular { .. }) => Ok(FileTypeQueryProduct {
                                file_type: Some(FileType::Regular),
                            }),
                            Some(FileEntryRef::Directory) => Ok(FileTypeQueryProduct {
                                file_type: Some(FileType::Directory),
                            }),
                            Some(FileEntryRef::Symlink(_)) => Ok(FileTypeQueryProduct {
                                file_type: Some(FileType::Symlink),
                            }),
                            None => Ok(FileTypeQueryProduct { file_type: None }),
                        }
                    }
                    Ok(OutputQueryProduct { reference: None }) => Ok(FileTypeQueryProduct { file_type: None }),
                    Err(err) => {
                        // FIXME: check if error corresponds to the file not existing
                        Ok(FileTypeQueryProduct { file_type: None })
                    }
                }
            } else if let Some(source_reference) = runner.reference_source_file(&runner.shared.query.label).await? {
                match source_reference.pre_observation_status() {
                    cealn_source::Status::Directory(_) => Ok(FileTypeQueryProduct {
                        file_type: Some(FileType::Directory),
                    }),
                    cealn_source::Status::File(_) => Ok(FileTypeQueryProduct {
                        file_type: Some(FileType::Regular),
                    }),
                    cealn_source::Status::Symlink(_) => Ok(FileTypeQueryProduct {
                        file_type: Some(FileType::Symlink),
                    }),
                    cealn_source::Status::NotFound => Ok(FileTypeQueryProduct { file_type: None }),
                }
            } else {
                Ok(FileTypeQueryProduct { file_type: None })
            }
        }
    }

    fn as_event_source(&self) -> BuildEventSource {
        BuildEventSource::InternalQuery
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!("query", kind = "file-type")
    }
}
