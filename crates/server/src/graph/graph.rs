use std::{mem::ManuallyDrop, path::PathBuf, sync::Arc, time::Instant};

use anyhow::{Context as _, Result};
use async_trait::async_trait;
use cealn_action_context::reqwest;
use cealn_data::{
    action::{ActionOutput, ConcreteAction},
    depmap::{ConcreteFiletreeType, DepmapHash, DepmapType},
    file_entry::{FileEntry, FileHash, FileHashRef},
    rule::BuildConfig,
};
use cealn_depset::{ConcreteFiletree, DepMap};
use cealn_event::EventContext;
use cealn_fs_materialize::{MaterializeCache, MaterializeContext};
use dashmap::{mapref::entry::Entry, DashMap};
use futures::{channel::oneshot, prelude::*};

use cealn_cache::{hot_disk, HotDiskCache};
use cealn_protocol::{
    event::BuildEventSource,
    query::{AllWorkspacesLoadQuery, AnalysisQuery, LoadQuery, OutputQuery, QueryType, RootWorkspaceLoadQuery},
};
use cealn_runtime::{interpreter, Interpreter};
use cealn_source::SourceMonitor;
use tracing::{debug, Span};

use super::BuildConfigId;
use crate::{
    executor::Executor,
    graph::node::{
        QueryNode, QueryRunnerDriver, _QueryRequest,
        action::{ActionQuery, ConcreteActionQuery},
        file_type::FileTypeQuery,
        output::OutputQueryRequest,
        package::LoadQueryRequest,
        rule::AnalysisQueryRequest,
        target::TargetQuery,
        target_exists::TargetExistsQuery,
        workspace::RootWorkspaceLoadQueryRequest,
    },
};

/// Manages an actively cached dependency graph of [`Query`]
#[derive(Clone)]
pub struct Graph(pub(crate) Arc<_Graph>);

struct RunningQueryHandle {
    _cancel_sender: Option<oneshot::Sender<()>>,
}

struct _BuildConfigRef {
    id: BuildConfigId,
    name: String,
    value: BuildConfig,
}

pub(crate) struct _Graph {
    pub(crate) source_view: SourceMonitor,
    pub(crate) executor: Executor,
    pub(super) cache_subsystem: Arc<CacheSubsystem>,
    pub(super) http_client: reqwest::Client,
    pub(super) temporary_directory: PathBuf,
    pub(super) materialize_cache: MaterializeCache,
    // TODO: set this up so it doesn't need exclusion once the interpreter is set up
    interpreter: Interpreter,

    pub(super) load_root_workspace: Arc<QueryNode<RootWorkspaceLoadQuery>>,
    pub(super) load_all_workspaces: Arc<QueryNode<AllWorkspacesLoadQuery>>,
    pub(super) output_queries: DashMap<OutputQuery, Arc<QueryNode<OutputQuery>>>,
    pub(super) analysis_queries: DashMap<AnalysisQuery, Arc<QueryNode<AnalysisQuery>>>,
    pub(super) load_queries: DashMap<LoadQuery, Arc<QueryNode<LoadQuery>>>,
    pub(super) action_queries: DashMap<ActionQuery, Arc<QueryNode<ActionQuery>>>,
    pub(super) concrete_action_queries: DashMap<ConcreteActionQuery, Arc<QueryNode<ConcreteActionQuery>>>,
    pub(super) target_queries: DashMap<TargetQuery, Arc<QueryNode<TargetQuery>>>,
    pub(super) file_type_queries: DashMap<FileTypeQuery, Arc<QueryNode<FileTypeQuery>>>,
    pub(super) target_exists_queries: DashMap<TargetExistsQuery, Arc<QueryNode<TargetExistsQuery>>>,
}

pub(super) struct CacheSubsystem {
    pub(super) primary_cache: Arc<HotDiskCache>,
    pub(super) depset_registry: cealn_depset::Registry,
}

pub trait GraphQuery {
    type Request;
}

pub(super) trait GraphQueryInternal: QueryType + Send + Sync {
    type Run<'a>: Future<Output = anyhow::Result<Self::Product>> + Send;

    fn get_query_node(graph: &_Graph, query: Self) -> Arc<QueryNode<Self>>;

    fn run<'a>(runner: &'a mut QueryRunnerDriver<Self>) -> Self::Run<'a>;

    fn construct_span(&self) -> Span;

    fn as_event_source(&self) -> BuildEventSource;
}

pub type QueryRequest<Q> = <Q as GraphQuery>::Request;

impl Graph {
    pub async fn new(
        source_view: SourceMonitor,
        executor: Executor,
        primary_cache: HotDiskCache,
        interpreter_options: interpreter::Options,
        temporary_directory: PathBuf,
        depset_registry: cealn_depset::Registry,
    ) -> Result<Self> {
        let primary_cache = Arc::new(primary_cache);
        let cache_subsystem = Arc::new(CacheSubsystem {
            primary_cache,
            depset_registry,
        });

        let interpreter = create_python_interpreter(&executor, interpreter_options).await?;

        let shared = Arc::new(_Graph {
            source_view,
            executor,
            materialize_cache: MaterializeCache::new(
                temporary_directory.join("cache/materialized"),
                cache_subsystem.clone(),
            ),
            cache_subsystem,
            http_client: reqwest::Client::builder()
                .pool_max_idle_per_host(0)
                .user_agent("cealn")
                .build()
                .unwrap(),
            temporary_directory,
            interpreter,
            load_root_workspace: QueryNode::new(RootWorkspaceLoadQuery {}),
            load_all_workspaces: QueryNode::new(AllWorkspacesLoadQuery {}),
            output_queries: Default::default(),
            analysis_queries: Default::default(),
            load_queries: Default::default(),
            action_queries: Default::default(),
            concrete_action_queries: Default::default(),
            target_queries: Default::default(),
            target_exists_queries: Default::default(),
            file_type_queries: Default::default(),
        });

        Ok(Graph(shared))
    }

    pub fn query_output(
        &self,
        query: OutputQuery,
        request_time: Instant,
        events: EventContext,
    ) -> QueryRequest<OutputQuery> {
        let request = self
            .0
            .get_or_create_query_request(query, &self.0.output_queries, request_time, events);
        OutputQueryRequest(request)
    }

    pub fn query_analysis(
        &self,
        query: AnalysisQuery,
        request_time: Instant,
        events: EventContext,
    ) -> QueryRequest<AnalysisQuery> {
        let request = self
            .0
            .get_or_create_query_request(query, &self.0.analysis_queries, request_time, events);
        AnalysisQueryRequest(request)
    }

    pub fn query_load(&self, query: LoadQuery, request_time: Instant, events: EventContext) -> QueryRequest<LoadQuery> {
        let request = self
            .0
            .get_or_create_query_request(query, &self.0.load_queries, request_time, events);
        LoadQueryRequest(request)
    }

    pub fn query_load_root_workspace(
        &self,
        request_time: Instant,
        events: EventContext,
    ) -> QueryRequest<RootWorkspaceLoadQuery> {
        let node = self.0.load_root_workspace.clone();
        let span = node.query.construct_span();
        RootWorkspaceLoadQueryRequest(_QueryRequest::new(self.0.clone(), node, request_time, events, span))
    }
}

impl _Graph {
    pub(super) fn get_or_create_query<Q>(
        &self,
        query: Q,
        collection: &DashMap<Q, Arc<QueryNode<Q>>>,
    ) -> Arc<QueryNode<Q>>
    where
        Q: GraphQueryInternal,
    {
        match collection.entry(query.clone()) {
            Entry::Occupied(entry) => entry.get().clone(),
            Entry::Vacant(entry) => {
                let node = QueryNode::new(query);
                entry.insert(node.clone());
                node
            }
        }
    }

    pub(super) fn get_or_create_query_request<Q>(
        self: &Arc<Self>,
        query: Q,
        collection: &DashMap<Q, Arc<QueryNode<Q>>>,
        request_time: Instant,
        events: EventContext,
    ) -> _QueryRequest<Q>
    where
        Q: GraphQueryInternal,
    {
        let node = self.get_or_create_query(query, collection);
        let span = node.query.construct_span();
        _QueryRequest::new(self.clone(), node, request_time, events, span)
    }

    #[tracing::instrument(level = "debug", err, skip(self))]
    pub(super) async fn get_python_interpreter(&self) -> anyhow::Result<Interpreter> {
        Ok(self.interpreter.clone())
    }

    pub(super) async fn write_action_cache(
        &self,
        action: &ConcreteAction,
        output: &ActionOutput,
    ) -> anyhow::Result<()> {
        self.cache_subsystem.primary_cache.write_action(action, output).await
    }

    pub(super) async fn lookup_action_cache(&self, action: &ConcreteAction) -> anyhow::Result<Option<ActionOutput>> {
        self.cache_subsystem.primary_cache.lookup_action(action).await
    }

    pub(super) async fn register_filetree(&self, depmap: ConcreteFiletree) -> anyhow::Result<DepmapHash> {
        let hash = self.cache_subsystem.depset_registry.register_filetree(depmap.clone());
        self.cache_subsystem
            .primary_cache
            .write_depmap::<ConcreteFiletreeType>(&depmap)
            .await?;
        Ok(hash)
    }

    #[tracing::instrument(level = "debug", err, skip(self))]
    pub(super) async fn lookup_filetree_cache(&self, hash: &DepmapHash) -> anyhow::Result<Option<ConcreteFiletree>> {
        self.cache_subsystem.lookup_filetree_cache(hash).await
    }

    #[tracing::instrument(level = "debug", err, skip(self))]
    pub(super) async fn open_cache_file(
        &self,
        content_hash: FileHashRef<'_>,
        executable: bool,
    ) -> anyhow::Result<Option<hot_disk::FileGuard>> {
        self.cache_subsystem
            .primary_cache
            .lookup_file(content_hash, executable)
            .await
    }
}

#[async_trait]
impl MaterializeContext for CacheSubsystem {
    async fn lookup_file<'a>(
        &'a self,
        digest: FileHashRef<'a>,
        executable: bool,
    ) -> anyhow::Result<Option<cealn_cache::hot_disk::FileGuard>> {
        self.primary_cache.lookup_file(digest, executable).await
    }

    async fn lookup_filetree_cache<'a>(&'a self, hash: &'a DepmapHash) -> anyhow::Result<Option<ConcreteFiletree>> {
        if let Some(depmap) = self.depset_registry.get_filetree(hash) {
            return Ok(Some(depmap));
        }
        debug!("depset registry miss");
        if let Some(depmap) = self.primary_cache.lookup_depmap::<ConcreteFiletreeType>(hash).await? {
            // Insert into registry so we don't have to load from disk in the future
            self.depset_registry.register_filetree(depmap.clone());
            return Ok(Some(depmap));
        }
        // FIXME: We need to ensure that we don't recycle depmaps that are flowing through the active dependency
        // graph, but this can also happen in the case where a depmap file in the cache has been deleted but the
        // referring actions have not.
        Ok(None)
    }
}

#[tracing::instrument(level = "info", err, skip(executor))]
async fn create_python_interpreter(
    executor: &Executor,
    options: interpreter::Options,
) -> Result<Interpreter, cealn_runtime_python_embed::CreateError> {
    executor
        .spawn_immediate(async move {
            let interpreter = cealn_runtime_python_embed::make_interpreter(options)?;

            Ok(interpreter)
        })
        .await
}
