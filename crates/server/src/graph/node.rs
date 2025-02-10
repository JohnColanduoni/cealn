pub(super) mod action;
pub(super) mod file_type;
pub(super) mod output;
pub(super) mod package;
pub(super) mod rule;
pub(super) mod target;
pub(super) mod target_exists;
pub(super) mod workspace;

use std::{
    borrow::Cow,
    convert::Infallible,
    fmt::{self, Debug, Display},
    fs::DirEntry,
    io::{Read, Write},
    mem::{self, ManuallyDrop},
    ops::{FromResidual, Try},
    path::PathBuf,
    pin::Pin,
    sync::{Arc, Weak},
    task::{Context, Poll, RawWaker, RawWakerVTable, Waker},
    time::Instant,
};

use anyhow::{anyhow, bail, Context as _};
use cealn_cache::{hash_serializable, hot_disk};
use cealn_data::{
    depmap::{ConcreteDepmapReference, DepmapHash},
    file_entry::{FileEntry, FileEntryRef, FileHash, FileHashRef, FileType},
    label::{self, LabelPath},
    Label, LabelBuf,
};
use cealn_depset::{ConcreteFiletree, DepMap};
use cealn_event::{BuildEventData, EventContext};
use cealn_protocol::{
    event::BuildEvent,
    query::{AllWorkspacesLoadQuery, AllWorkspacesLoadQueryProduct, QueryType},
};
use cealn_runtime::Interpreter;
use cealn_source::SourceReference;
use cealn_source_fs::{SourceFs, SourceReferenceCollector, SourceReferenceHandler};
use compio_core::buffer::AllowTake;
use compio_fs::File;
use futures::{
    channel::mpsc,
    future::{BoxFuture, RemoteHandle},
    pin_mut,
    prelude::*,
    select,
    stream::FuturesUnordered,
};
use hyper::http::request;
use parking_lot::Mutex;
use pin_project::pin_project;
use slab::Slab;
use tracing::{debug_span, info, Instrument, Span};
use weak_table::PtrWeakHashSet;

use crate::{
    error::QueryCallErrorContext,
    graph::{
        graph::{GraphQueryInternal, _Graph},
        QueryResult,
    },
    vfs::named_workspaces::NamedWorkspacesFs,
};

pub(super) struct _QueryRequest<Q: GraphQueryInternal> {
    shared: Arc<QueryNode<Q>>,
    graph: Arc<_Graph>,

    request_time: Instant,
    events: EventContext,

    waker_index: Option<RegisteredWaker>,
    have_triggered_source_check: bool,
    have_registered_interest: Option<usize>,
    finished: bool,

    span: Span,
}

struct QueryCheck<Q: GraphQueryInternal> {
    shared: Arc<QueryNode<Q>>,
    graph: Arc<_Graph>,

    waker_index: Option<RegisteredWaker>,
    have_trigged_source_check: bool,
    have_registered_interest: Option<usize>,

    request_time: Instant,
    events: EventContext,
    expected_cached_run_id: usize,
}

impl<Q: GraphQueryInternal> Unpin for _QueryRequest<Q> {}

pub(super) struct QueryNode<Q: QueryType> {
    pub(super) query: Q,
    state: Mutex<QueryState<Q>>,
    back_references: parking_lot::RwLock<PtrWeakHashSet<Weak<dyn GenQueryNode>>>,
}

struct QueryState<Q: QueryType> {
    next_run_id: usize,
    kind: QueryStateKind<Q>,
}

enum QueryStateKind<Q: QueryType> {
    Empty,
    Running(RunningQueryWakers),
    Checking(RunningQueryWakers),
    Cached(CachedQueryResult<Q>),
}

struct CachedQueryResult<Q: QueryType> {
    run_id: usize,
    last_check_time: Instant,

    result: QueryResult<Q::Product>,
    source_references: Vec<SourceReference>,
    query_references: Vec<QueryReference>,

    known_dirty_state: Option<bool>,
}

struct QueryReference {
    node: Arc<dyn GenQueryNode>,
    output_hash: Option<[u8; 32]>,
    cached_run_id: usize,
}

struct RunningQueryWakers {
    run_id: usize,
    wakers: Slab<Waker>,
    interest_count: usize,
}

struct RegisteredWaker {
    run_id: usize,
    slab_index: usize,
}

pub(super) struct QueryRunner<Q: GraphQueryInternal> {
    driver: QueryRunnerDriver<Q>,
}

pub(super) struct QueryRunnerDriver<Q: GraphQueryInternal> {
    shared: Arc<QueryNode<Q>>,
    graph: Arc<_Graph>,

    run_id: usize,
    request_time: Instant,
    cached_result: Option<CachedQueryResult<Q>>,

    // TODO: The futures where we access this structure don't actually get sent to other threads so it would be nice
    // to be able to use `RefCell` here. We should see if we can thread that needle.
    events_storage: Mutex<Vec<BuildEvent>>,
    events_live: EventContext,
    source_references: SourceReferenceCollector,
    query_references: Mutex<Vec<QueryReference>>,
    sent_start_event: bool,
}

struct QueryChecker<Q: GraphQueryInternal> {
    shared: Arc<QueryNode<Q>>,
    graph: Arc<_Graph>,

    request_time: Instant,
    cached_result: CachedQueryResult<Q>,
    events: EventContext,
}

impl<Q: GraphQueryInternal> _QueryRequest<Q> {
    pub(super) fn new(
        graph: Arc<_Graph>,
        node: Arc<QueryNode<Q>>,
        request_time: Instant,
        events: EventContext,
        span: Span,
    ) -> Self {
        _QueryRequest {
            shared: node,
            graph,
            request_time,
            events,
            waker_index: None,
            have_triggered_source_check: false,
            have_registered_interest: None,
            finished: false,
            span,
        }
    }
}

impl<Q: GraphQueryInternal> QueryNode<Q> {
    pub(super) fn new(query: Q) -> Arc<Self> {
        Arc::new(QueryNode {
            query,
            state: Mutex::new(QueryState {
                next_run_id: 1,
                kind: QueryStateKind::Empty,
            }),
            back_references: Default::default(),
        })
    }

    async fn check_up_to_date(
        self: Arc<Self>,
        graph: Arc<_Graph>,
        run_id: usize,
        request_time: Instant,
        events: EventContext,
    ) -> bool {
        let check = QueryCheck {
            shared: self,
            graph,
            waker_index: None,
            have_trigged_source_check: false,
            have_registered_interest: None,
            request_time,
            events,
            expected_cached_run_id: run_id,
        };

        check.await
    }

    async fn check_cached_result(
        &self,
        graph: &Arc<_Graph>,
        cached_result: &CachedQueryResult<Q>,
        request_time: Instant,
        events: EventContext,
    ) -> anyhow::Result<bool> {
        if cached_result.result.output().is_err() {
            // Don't serve from cache on error

            return Ok(false);
        }

        // First, check if this cached result has been validated at or after the request time. This ensures that we
        // don't duplicate checks when a dependency shows up in multiple places in the dependency tree.
        if request_time <= cached_result.last_check_time {
            return Ok(true);
        }

        // let mut root_events = events.fork();
        // root_events.send(BuildEventData::CacheCheckStart);

        let check_queries_span = debug_span!("check_queries");
        let check_queries = async {
            let mut up_to_date = true;
            let mut check_futures = FuturesUnordered::new();

            let mut query_references_iter = cached_result.query_references.iter();

            loop {
                while check_futures.len() < 32 {
                    let Some(query) = query_references_iter.next() else {
                        break;
                    };
                    check_futures.push(query.check_up_to_date(graph.clone(), request_time, events.fork()));
                }

                let Some(query_up_to_date) = check_futures.next().await else {
                    break;
                };

                if !query_up_to_date {
                    up_to_date = false;
                    break;
                }
            }

            Ok(up_to_date)
        }
        .instrument(check_queries_span)
        .fuse();
        let check_files_span = debug_span!("check_files");
        let check_files = {
            let mut events = events.fork();
            let cached_result = &cached_result;
            async move {
                let mut up_to_date = true;
                let mut check_futures = FuturesUnordered::new();

                let mut source_references_iter = cached_result.source_references.iter();

                loop {
                    while check_futures.len() < 128 {
                        let Some(file) = source_references_iter.next() else {
                            break;
                        };
                        check_futures.push(file.has_changed_until(request_time));
                    }

                    let Some(changed) = check_futures.try_next().await? else {
                        break;
                    };

                    if changed {
                        up_to_date = false;
                        break;
                    }
                }
                Ok(up_to_date)
            }
        }
        .instrument(check_files_span)
        .fuse();

        pin_mut!(check_queries);
        pin_mut!(check_files);

        // Short circuit if either check find something out of date
        let result = select! {
            check_queries_result = check_queries => {
                if !check_queries_result? {
                    Ok(false)
                } else {
                    check_files.await
                }
            },
            check_files_result = check_files => {
                if !check_files_result? {
                    Ok(false)
                } else {
                    check_queries.await
                }
            }
        };

        // root_events.send(BuildEventData::CacheCheckEnd);

        result
    }

    fn complete_run(&self, cached_result: CachedQueryResult<Q>) {
        let mut state = self.state.lock();

        match mem::replace(&mut state.kind, QueryStateKind::Cached(cached_result)) {
            QueryStateKind::Running(wake_state) => {
                // Release lock before we wake futures
                mem::drop(state);

                for (_, waker) in wake_state.wakers {
                    waker.wake();
                }
            }
            _ => unreachable!(),
        }
    }

    #[tracing::instrument(level = "debug", skip_all, fields(query=?self.query))]
    fn mark_dirty(&self) {
        {
            let mut state = self.state.lock();
            let QueryStateKind::Cached(cache_result) = &mut state.kind else {
                return;
            };
            if cache_result.known_dirty_state.is_none() {
                // Query does not support lazy invalidation, so there's no point in continuing
                return;
            }
            cache_result.known_dirty_state = Some(true);
        }
        let mut backreferences = self.back_references.read();
        // FIXME: consume backreferences once we fix re-validation of backreferences on cache hit
        for backreference in backreferences.iter() {
            backreference.gen_mark_dirty();
        }
    }

    fn dirty_waker(self: &Arc<Self>) -> Waker {
        unsafe fn dirty_waker_clone<Q: QueryType + GraphQueryInternal>(ptr: *const ()) -> RawWaker {
            let weak = ManuallyDrop::new(Weak::from_raw(ptr as *const QueryNode<Q>));
            let clone = (&*weak).clone();
            RawWaker::new(
                Weak::into_raw(clone) as _,
                &RawWakerVTable::new(
                    dirty_waker_clone::<Q>,
                    dirty_waker_wake::<Q>,
                    dirty_waker_wake_by_ref::<Q>,
                    dirty_waker_drop::<Q>,
                ),
            )
        }

        unsafe fn dirty_waker_wake<Q: QueryType + GraphQueryInternal>(ptr: *const ()) {
            let weak = Weak::from_raw(ptr as *const QueryNode<Q>);
            if let Some(node) = weak.upgrade() {
                node.mark_dirty();
            }
        }

        unsafe fn dirty_waker_wake_by_ref<Q: QueryType + GraphQueryInternal>(ptr: *const ()) {
            let weak = ManuallyDrop::new(Weak::from_raw(ptr as *const QueryNode<Q>));
            if let Some(node) = weak.upgrade() {
                node.mark_dirty();
            }
        }

        unsafe fn dirty_waker_drop<Q: QueryType + GraphQueryInternal>(ptr: *const ()) {
            mem::drop(Weak::from_raw(ptr as *const QueryNode<Q>));
        }

        unsafe {
            Waker::from_raw(RawWaker::new(
                Weak::into_raw(Arc::downgrade(self)) as _,
                &RawWakerVTable::new(
                    dirty_waker_clone::<Q>,
                    dirty_waker_wake::<Q>,
                    dirty_waker_wake_by_ref::<Q>,
                    dirty_waker_drop::<Q>,
                ),
            ))
        }
    }
}

impl<Q: GraphQueryInternal> QueryRunner<Q> {
    fn new(
        node: Arc<QueryNode<Q>>,
        graph: Arc<_Graph>,
        run_id: usize,
        request_time: Instant,
        events: EventContext,
        cached_result: Option<CachedQueryResult<Q>>,
    ) -> Self {
        QueryRunner {
            driver: QueryRunnerDriver {
                shared: node,
                graph,

                run_id,
                request_time,
                cached_result,

                // FIXME: have event context store both locally and push to live events
                events_storage: Default::default(),
                events_live: events,
                source_references: Default::default(),
                query_references: Default::default(),
                sent_start_event: false,
            },
        }
    }

    #[tracing::instrument("Query::run", level = "info", skip_all, fields(query.kind = Q::KIND, query.label, cache.status, cache.existed))]
    async fn run(mut self, parent_span: Span) {
        let current_span = Span::current();
        current_span.follows_from(parent_span);
        if let Some(label) = self.driver.shared.query.label() {
            current_span.record("query.label", label.as_str());
        }

        if let Some(cached_result) = &self.driver.cached_result {
            current_span.record("cache.existed", "true");
            // Check if this cached result is up to date and can be used to satisfy the query
            let up_to_date = match self
                .driver
                .shared
                .check_cached_result(
                    &self.driver.graph,
                    cached_result,
                    self.driver.request_time,
                    self.driver.events_live.fork(),
                )
                .await
            {
                Ok(up_to_date) => up_to_date,
                Err(err) => {
                    self.driver.shared.complete_run(CachedQueryResult {
                        run_id: self.driver.run_id,
                        last_check_time: self.driver.request_time,
                        result: QueryResult::new(
                            Err(err),
                            // TODO: add event here indicating checking cache failed
                            vec![],
                            self.driver.run_id,
                        ),
                        source_references: Vec::new(),
                        query_references: Vec::new(),
                        // FIXME: calculate this based on actual watch state
                        known_dirty_state: if cfg!(target_os = "linux") && false {
                            Some(false)
                        } else {
                            None
                        },
                    });
                    return;
                }
            };
            if up_to_date {
                // Refresh our watches and references, as they may have been invalidated
                for reference in &cached_result.source_references {
                    let can_wake = reference.wake_on_changed(|| self.driver.shared.dirty_waker());
                    if !can_wake {
                        todo!()
                    }
                }
                for reference in &cached_result.query_references {
                    reference.node.add_backreference(self.driver.shared.clone());
                }

                current_span.record("cache.status", "hit");
                // All dependencies are unchanged, just keep the previous cached result
                let cached_result = self.driver.cached_result.take().unwrap();
                self.driver.shared.complete_run(cached_result);
                return;
            }
        } else {
            current_span.record("cache.existed", "false");
        }
        current_span.record("cache.status", "miss");

        let output = Q::run(&mut self.driver).await;
        if self.driver.sent_start_event {
            self.driver.events().send(BuildEventData::QueryRunEnd);
        }

        // FIXME: check if any of the source references changed since creation. If so, invalidate result and run again.

        let cached_result = CachedQueryResult {
            run_id: self.driver.run_id,
            // Since we passed this time to the cache check above, we can only assert that this result is at least that
            // fresh.
            last_check_time: self.driver.request_time,
            result: QueryResult::new(output, self.driver.events_storage.into_inner(), self.driver.run_id),
            source_references: self
                .driver
                .source_references
                .try_unwrap()
                .unwrap_or_else(|_| panic!("unexpected remaining references")),
            query_references: self.driver.query_references.into_inner(),
            // FIXME: calculate this based on actual watch state
            known_dirty_state: if cfg!(target_os = "linux") && false {
                Some(false)
            } else {
                None
            },
        };

        self.driver.shared.complete_run(cached_result);
    }
}

impl<Q: GraphQueryInternal> QueryRunnerDriver<Q> {
    async fn query<D: GraphQueryInternal>(&self, query: D) -> NestedQueryCallResult<D::Product> {
        let span = query.construct_span();
        let node = D::get_query_node(&self.graph, query);
        let result = _QueryRequest::new(
            self.graph.clone(),
            node.clone(),
            self.request_time,
            self.events_live.fork(),
            span,
        )
        .await;
        self.push_query_reference(QueryReference {
            node: node.clone(),
            output_hash: result
                .output()
                .as_ref()
                .ok()
                .map(|x| hash_serializable(x).as_ref().try_into().unwrap()),
            cached_run_id: result.run_id(),
        });
        NestedQueryCallResult {
            result,
            called_query: node.clone(),
            calling_query: self.shared.clone(),
        }
    }

    async fn query_speculatve<D: GraphQueryInternal>(&self, query: D) -> QueryResult<D::Product> {
        let (event_context, events) = EventContext::new();
        let span = query.construct_span();
        let node = D::get_query_node(&self.graph, query);
        let result = _QueryRequest::new(self.graph.clone(), node.clone(), self.request_time, event_context, span).await;
        // FIXME: relay events on success
        // FIXME: register for existince even on failure
        self.push_query_reference(QueryReference {
            node,
            output_hash: result
                .output()
                .as_ref()
                .ok()
                .map(|x| hash_serializable(x).as_ref().try_into().unwrap()),
            cached_run_id: result.run_id(),
        });
        result
    }

    fn push_query_reference(&self, reference: QueryReference) {
        reference.node.add_backreference(self.shared.clone());
        self.query_references.lock().push(reference);
    }

    /// Attempts to reference a source file by label
    ///
    /// Returns `None` if the file does not exit or the label refers to a non-local workspace.
    async fn reference_source_file(&self, file_label: &Label) -> anyhow::Result<Option<SourceReference>> {
        let root_workspace_label = match file_label.split_root() {
            // TODO: should we consider workspace relative labels here to automatically refer to the root workspace?
            (label::Root::WorkspaceRelative, _) => Cow::Borrowed(file_label),
            (label::Root::Workspace(workspace_name), tail) => {
                // TODO: should we cache this and not add it to the requested queries each time a source file is referenced?
                let workspaces = self.query(AllWorkspacesLoadQuery {}).await;
                let workspaces = workspaces.output_ref()?;

                if workspace_name == workspaces.name {
                    Cow::Owned(Label::ROOT.join(tail).unwrap())
                } else {
                    let local_workspace = workspaces
                        .local_workspaces
                        .iter()
                        .find(|local_workspace| local_workspace.name == workspace_name)
                        .ok_or_else(|| anyhow!("label {:?} refers to an unknown workspace", file_label))?;

                    let mut root_workspace_label = Label::ROOT.join(&local_workspace.path).unwrap();
                    root_workspace_label.push(tail).unwrap();
                    Cow::Owned(root_workspace_label)
                }
            }
            (label::Root::PackageRelative, _) => bail!("attempted to reference package-relative file"),
        };
        let root_workspace_label = root_workspace_label.normalize()?;
        let file = self.graph.source_view.reference(&root_workspace_label).await?;
        self.source_references.push(file.clone());
        let wake_supported = file.wake_on_changed(|| self.shared.dirty_waker());
        if !wake_supported {
            todo!()
        }
        if !file.existed() {
            // The file does not exist, but it might in the future so we still add it to our source references.
            return Ok(None);
        }
        Ok(Some(file))
    }

    async fn reference_source_file_as_depmap(
        &self,
        file_label: &Label,
    ) -> anyhow::Result<Option<ConcreteDepmapReference>> {
        let file_label = file_label.normalize()?;

        let Some(source_reference) = self.reference_source_file(&file_label).await? else {
            return Ok(None);
        };

        match source_reference.pre_observation_status() {
            cealn_source::Status::Directory(_) => {
                let source_root = source_reference.full_file_path();

                // FIXME: this is an inefficient way of handling local builds
                let mut depmap = DepMap::builder();

                let mut entries = source_reference.reference_children().await?;

                while let Some(entry) = entries.pop() {
                    let Some(subpath) = entry.full_file_path().strip_prefix(source_reference.full_file_path()).ok().and_then(|x| x.to_str()).and_then(|x| LabelPath::new(x).ok()) else {
                        continue;
                    };
                    let subpath = subpath.normalize_require_descending().context("path escapes root")?;

                    match entry.pre_observation_status() {
                        cealn_source::Status::Directory(_) => {
                            depmap.insert(subpath.as_ref(), FileEntryRef::Directory);

                            // Add entries to stack
                            entries.extend(entry.reference_children().await?);
                        }
                        cealn_source::Status::File(_) => {
                            let (content_hash, executable) = self
                                .graph
                                .cache_subsystem
                                .primary_cache
                                .move_to_cache_named(entry.full_file_path(), false)
                                .await?;
                            depmap.insert(
                                subpath.as_ref(),
                                FileEntry::Regular {
                                    content_hash,
                                    executable,
                                }
                                .as_ref(),
                            );
                        }
                        cealn_source::Status::Symlink(state) => {
                            depmap.insert(subpath.as_ref(), FileEntryRef::Symlink(&state.target));
                        }
                        cealn_source::Status::NotFound => {}
                    }

                    self.source_references.push(entry);
                }

                let depmap_hash = self.graph.register_filetree(depmap.build()).await?;
                Ok(Some(ConcreteDepmapReference {
                    hash: depmap_hash,
                    subpath: None,
                }))
            }
            cealn_source::Status::File(file_info) => {
                let source_file_path = file_label.source_file_path().context("not a source file path")?;
                let source_file_name = source_file_path.file_name_normalized().context("path escapes root")?;

                // FIXME: this is an inefficient way of handling local builds
                let existing_entry = self
                    .graph
                    .cache_subsystem
                    .primary_cache
                    .lookup_file(file_info.hash.as_ref(), file_info.executable)
                    .await?;
                if let Some(_) = existing_entry {
                    let depmap = ConcreteFiletree::builder()
                        .insert(
                            source_file_name,
                            FileEntry::Regular {
                                content_hash: file_info.hash.clone(),
                                executable: file_info.executable,
                            }
                            .as_ref(),
                        )
                        .build();

                    let depmap_hash = self.graph.register_filetree(depmap).await?;
                    Ok(Some(ConcreteDepmapReference {
                        hash: depmap_hash,
                        subpath: Some(source_file_name.to_owned()),
                    }))
                } else {
                    let mut cachefile =
                        cealn_fs::tempfile(&self.graph.temporary_directory, "source-copy", file_info.executable)
                            .await?;
                    let copied_digest;
                    {
                        // FIXME: handle race here where file is deleted or replaced with a directory since observation
                        let mut source_file = File::open(source_reference.full_file_path()).await?;
                        let mut cachefile = cachefile.ensure_open().await?;
                        let mut buffer = Vec::with_capacity(128 * 1024);
                        let mut hasher = ring::digest::Context::new(&ring::digest::SHA256);
                        loop {
                            let read_len = source_file.read(AllowTake(&mut buffer)).await?;
                            if read_len == 0 {
                                break;
                            }
                            hasher.update(&buffer);
                            cachefile.write_all_mono(&mut buffer).await?;
                            buffer.truncate(0);
                        }
                        copied_digest = FileHash::Sha256(hasher.finish().as_ref().try_into().unwrap());
                    }
                    if copied_digest != file_info.hash {
                        // We read in an intermediate state, retry
                        todo!()
                    }
                    self.graph
                        .cache_subsystem
                        .primary_cache
                        .move_to_cache_prehashed(cachefile, copied_digest.as_ref(), file_info.executable)
                        .await?;

                    let depmap = ConcreteFiletree::builder()
                        .insert(
                            source_file_name,
                            FileEntry::Regular {
                                content_hash: copied_digest,
                                executable: file_info.executable,
                            }
                            .as_ref(),
                        )
                        .build();

                    let depmap_hash = self.graph.register_filetree(depmap).await?;
                    Ok(Some(ConcreteDepmapReference {
                        hash: depmap_hash,
                        subpath: Some(source_file_name.to_owned()),
                    }))
                }
            }
            cealn_source::Status::Symlink(_) => todo!(),
            cealn_source::Status::NotFound => todo!(),
        }
    }

    async fn get_sourcefs(&self, root: &Label) -> anyhow::Result<SourceFs> {
        let root = self.graph.source_view.reference(root).await?;
        let source_fs = SourceFs::new(root, self.source_references.clone()).await?;
        Ok(source_fs)
    }

    #[tracing::instrument(level = "debug", err, skip(self, info))]
    async fn build_workspaces_fs(&self, info: &AllWorkspacesLoadQueryProduct) -> anyhow::Result<NamedWorkspacesFs> {
        let mut builder = NamedWorkspacesFs::builder();
        let root_source_fs = self.get_sourcefs(Label::ROOT).await?;
        builder.add_source_fs(info.name.clone(), root_source_fs);
        for local_workspace in info.local_workspaces.iter() {
            let local_source_fs = self.get_sourcefs(&local_workspace.path).await?;
            builder.add_source_fs(local_workspace.name.clone(), local_source_fs);
        }
        Ok(builder.build())
    }

    async fn canonicalize_label(&mut self, label: &Label) -> anyhow::Result<Option<LabelBuf>> {
        let workspaces = self.query(AllWorkspacesLoadQuery {}).await;
        let workspaces = workspaces.output_ref()?;

        // First canonicalize relative root
        let label = match label.split_root() {
            (label::Root::Workspace(_), _) => Cow::Borrowed(label),
            (label::Root::WorkspaceRelative, tail) => {
                let mut rooted = LabelBuf::new(format!("@{}//", workspaces.name))?;
                rooted.push(tail).unwrap();
                Cow::Owned(rooted)
            }
            (label::Root::PackageRelative, _) => bail!("unexpected package relative label"),
        };

        // FIXME: canonicalize paths within local workspaces to those workspaces, instead of containing workspace

        match label.normalize()? {
            Cow::Owned(value) => Ok(Some(value)),
            Cow::Borrowed(_) => match label {
                Cow::Owned(value) => Ok(Some(value)),
                Cow::Borrowed(_) => Ok(None),
            },
        }
    }

    async fn reference_cache_file<'a>(
        &self,
        digest: FileHashRef<'a>,
        executable: bool,
    ) -> anyhow::Result<hot_disk::FileGuard> {
        self.graph
            .cache_subsystem
            .primary_cache
            .lookup_file(digest, executable)
            .await?
            .ok_or_else(|| anyhow!("file not found for hash {:?} (executable: {})", digest, executable))
    }

    async fn get_python_interpreter(&self) -> anyhow::Result<Interpreter> {
        self.graph.get_python_interpreter().await
    }

    fn get_action_context(&self) -> crate::graph::action::Context {
        crate::graph::action::Context::new(self.graph.clone(), self.events_live.fork())
    }

    #[inline]
    fn events(&mut self) -> &mut EventContext {
        &mut self.events_live
    }
}

impl<Q: GraphQueryInternal> QueryChecker<Q> {
    #[tracing::instrument("Query::check", level = "info", skip_all, fields(query.kind = Q::KIND, query.label))]
    async fn run(mut self, parent_span: Span) {
        let current_span = Span::current();
        current_span.follows_from(parent_span);
        if let Some(label) = self.shared.query.label() {
            current_span.record("query.label", label.as_str());
        }

        // Check if this cached result is up to date and can be used to satisfy the query
        let up_to_date = match self
            .shared
            .check_cached_result(&self.graph, &self.cached_result, self.request_time, self.events.fork())
            .await
        {
            Ok(up_to_date) => up_to_date,
            Err(_) => todo!(),
        };

        let state_kind = if up_to_date {
            self.cached_result.last_check_time = self.request_time;
            QueryStateKind::Cached(self.cached_result)
        } else {
            // Cache was invalid, drop it
            QueryStateKind::Empty
        };

        let mut state = self.shared.state.lock();

        match mem::replace(&mut state.kind, state_kind) {
            QueryStateKind::Checking(wake_state) => {
                // Release lock before we wake futures
                mem::drop(state);

                for (_, waker) in wake_state.wakers {
                    waker.wake();
                }
            }
            _ => unreachable!(),
        }
    }
}

impl<Q: GraphQueryInternal> Drop for _QueryRequest<Q> {
    fn drop(&mut self) {
        if !self.finished {
            if let Some(run_id) = self.have_registered_interest {
                let mut state = self.shared.state.lock();
                match state.kind {
                    QueryStateKind::Running(ref mut wake_state) if run_id == wake_state.run_id => {
                        wake_state.interest_count -= 1;
                    }
                    _ => {}
                }
            }
        }
    }
}

impl<Q: GraphQueryInternal> Future for _QueryRequest<Q> {
    type Output = QueryResult<Q::Product>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let _guard = this.span.enter();

        let mut state = this.shared.state.lock();
        match state.kind {
            QueryStateKind::Empty => {}
            QueryStateKind::Running(ref mut wake_state) | QueryStateKind::Checking(ref mut wake_state) => {
                if this.have_registered_interest != Some(wake_state.run_id) {
                    wake_state.interest_count += 1;
                    this.have_registered_interest = Some(wake_state.run_id);
                }
                if let Some(RegisteredWaker { run_id, slab_index }) = this.waker_index {
                    if wake_state.run_id == run_id {
                        let existing_waker = &mut wake_state.wakers[slab_index];
                        if !existing_waker.will_wake(cx.waker()) {
                            *existing_waker = cx.waker().clone();
                        }
                        return Poll::Pending;
                    }
                }
                let slab_index = wake_state.wakers.insert(cx.waker().clone());
                this.waker_index = Some(RegisteredWaker {
                    run_id: wake_state.run_id,
                    slab_index,
                });
                return Poll::Pending;
            }
            QueryStateKind::Cached(ref cached_result) => {
                if cached_result.known_dirty_state == Some(false) {
                    this.finished = true;
                    return Poll::Ready(cached_result.result.clone());
                }
                if this.have_triggered_source_check || cached_result.last_check_time >= this.request_time {
                    // We've already performed a cache check some point after this future's creation, so we can return
                    // the cached result.
                    this.finished = true;
                    return Poll::Ready(cached_result.result.clone());
                } else {
                    // To fulfill the contract of this future, we need to check sources at some point after this future
                    // was created. Trigger another run of this query
                }
            }
        }

        // If we get here, this query is not running at the moment but we need it to run to satisfy this future for one
        // reason or another. Trigger a new run.
        let run_id = state.next_run_id;
        state.next_run_id += 1;
        let mut wakers = Slab::with_capacity(1);
        let slab_index = wakers.insert(cx.waker().clone());
        this.waker_index = Some(RegisteredWaker { run_id, slab_index });
        let new_state = QueryStateKind::Running(RunningQueryWakers {
            run_id,
            wakers,
            interest_count: 1,
        });
        this.have_registered_interest = Some(run_id);
        let cached_result = match mem::replace(&mut state.kind, new_state) {
            QueryStateKind::Cached(cached) => Some(cached),
            _ => None,
        };

        let mut events = this.events.fork();
        events.set_source(this.shared.query.as_event_source());
        let runner = QueryRunner::new(
            this.shared.clone(),
            this.graph.clone(),
            run_id,
            this.request_time,
            events,
            cached_result,
        );
        let parent_span = Span::current();
        this.graph.executor.spawn(QueryRunWrapper {
            node: this.shared.clone(),
            future: runner.run(parent_span),
        });

        this.have_triggered_source_check = true;
        Poll::Pending
    }
}

impl<Q: GraphQueryInternal> Future for QueryCheck<Q> {
    type Output = bool;

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let mut state = this.shared.state.lock();
        match state.kind {
            QueryStateKind::Empty => {
                // Cache was invalidated or never filled to begin with
                return Poll::Ready(false);
            }
            QueryStateKind::Running(ref mut wake_state) | QueryStateKind::Checking(ref mut wake_state) => {
                if this.have_registered_interest != Some(wake_state.run_id) {
                    wake_state.interest_count += 1;
                    this.have_registered_interest = Some(wake_state.run_id);
                }
                if let Some(RegisteredWaker { run_id, slab_index }) = this.waker_index {
                    if wake_state.run_id == run_id {
                        let existing_waker = &mut wake_state.wakers[slab_index];
                        if !existing_waker.will_wake(cx.waker()) {
                            *existing_waker = cx.waker().clone();
                        }
                        return Poll::Pending;
                    }
                }
                let slab_index = wake_state.wakers.insert(cx.waker().clone());
                this.waker_index = Some(RegisteredWaker {
                    run_id: wake_state.run_id,
                    slab_index,
                });
                return Poll::Pending;
            }
            QueryStateKind::Cached(ref cached_result) => {
                if cached_result.known_dirty_state == Some(false) {
                    return Poll::Ready(true);
                }
                if this.have_trigged_source_check || cached_result.last_check_time >= this.request_time {
                    // We've already performed a cache check some point after the request time, so we can return
                    // the cached result.
                    return Poll::Ready(cached_result.run_id == this.expected_cached_run_id);
                } else {
                    // To fulfill the contract of this future, we need to check sources at some point after this future
                    // was created. Trigger another run of this query
                }
            }
        }

        // If we get here, this query is not running at the moment but we need it to run to satisfy this future for one
        // reason or another. Trigger a new run.
        let run_id = state.next_run_id;
        state.next_run_id += 1;
        let mut wakers = Slab::with_capacity(1);
        let slab_index = wakers.insert(cx.waker().clone());
        this.waker_index = Some(RegisteredWaker { run_id, slab_index });
        let new_state = QueryStateKind::Checking(RunningQueryWakers {
            run_id,
            wakers,
            interest_count: 1,
        });
        this.have_registered_interest = Some(run_id);
        let cached_result = match mem::replace(&mut state.kind, new_state) {
            QueryStateKind::Cached(cached) => cached,
            _ => unreachable!(),
        };

        let mut events = this.events.fork();
        events.set_source(this.shared.query.as_event_source());
        let checker = QueryChecker {
            shared: this.shared.clone(),
            graph: this.graph.clone(),
            request_time: this.request_time,
            cached_result,
            events,
        };
        let parent_span = Span::current();
        this.graph.executor.spawn(QueryRunWrapper {
            node: this.shared.clone(),
            future: checker.run(parent_span),
        });

        this.have_trigged_source_check = true;
        Poll::Pending
    }
}

#[pin_project]
struct QueryRunWrapper<Q: GraphQueryInternal, F: Future> {
    node: Arc<QueryNode<Q>>,
    #[pin]
    future: F,
}

impl<Q: GraphQueryInternal, F: Future<Output = ()>> Future for QueryRunWrapper<Q, F> {
    type Output = F::Output;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.project();

        // TODO: probably don't want to lock here
        {
            let mut state = this.node.state.lock();
            match &state.kind {
                QueryStateKind::Running(waker_state) => {
                    if waker_state.interest_count == 0 {
                        info!(query = ?&this.node.query, "cancelled query");
                        state.kind = QueryStateKind::Empty;
                        return Poll::Ready(());
                    }
                }
                _ => {}
            }
        }

        this.future.poll(cx)
    }
}

impl QueryReference {
    async fn check_up_to_date(&self, graph: Arc<_Graph>, request_time: Instant, events: EventContext) -> bool {
        let definitely_up_to_date = self
            .node
            .clone()
            .check_up_to_date(graph.clone(), self.cached_run_id, request_time, events.fork())
            .await;
        if definitely_up_to_date {
            return true;
        }
        let Some(previous_hash) = self.output_hash else {
            return false;
        };
        // Re-run query and see if the output hash matches
        let Some(new_hash) = self.node.clone().query_and_hash(graph, request_time, events).await else {
            return false;
        };
        previous_hash == new_hash
    }
}

pub(crate) trait GenQueryNode: Send + Sync + Debug {
    fn check_up_to_date(
        self: Arc<Self>,
        graph: Arc<_Graph>,
        run_id: usize,
        request_time: Instant,
        events: EventContext,
    ) -> BoxFuture<'static, bool>;

    fn query_and_hash(
        self: Arc<Self>,
        graph: Arc<_Graph>,
        request_time: Instant,
        events: EventContext,
    ) -> BoxFuture<'static, Option<[u8; 32]>>;

    fn gen_mark_dirty(&self);

    fn add_backreference(&self, node: Arc<dyn GenQueryNode>);
}

impl<Q: GraphQueryInternal> GenQueryNode for QueryNode<Q> {
    fn check_up_to_date(
        self: Arc<Self>,
        graph: Arc<_Graph>,
        run_id: usize,
        request_time: Instant,
        events: EventContext,
    ) -> BoxFuture<'static, bool> {
        QueryNode::check_up_to_date(self, graph, run_id, request_time, events).boxed()
    }

    fn query_and_hash(
        self: Arc<Self>,
        graph: Arc<_Graph>,
        request_time: Instant,
        events: EventContext,
    ) -> BoxFuture<'static, Option<[u8; 32]>> {
        async move {
            let span = Q::construct_span(&self.query);
            let result = _QueryRequest::new(graph, self, request_time, events, span).await;
            let Ok(product) = result.output().as_ref() else {
            return None;
        };
            Some(hash_serializable(product).as_ref().try_into().unwrap())
        }
        .boxed()
    }

    fn gen_mark_dirty(&self) {
        QueryNode::<Q>::mark_dirty(self);
    }

    fn add_backreference(&self, node: Arc<dyn GenQueryNode>) {
        let mut backreferences = self.back_references.write();
        backreferences.insert(node);
    }
}

impl<Q: GraphQueryInternal> fmt::Debug for QueryNode<Q> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.query, f)
    }
}
struct ExpandedEntry {
    entry: DirEntry,
    path: PathBuf,
}

impl From<DirEntry> for ExpandedEntry {
    fn from(entry: DirEntry) -> Self {
        ExpandedEntry {
            path: entry.path(),
            entry,
        }
    }
}

pub struct NestedQueryCallResult<T> {
    result: QueryResult<T>,
    called_query: Arc<dyn GenQueryNode>,
    calling_query: Arc<dyn GenQueryNode>,
}

impl<T> NestedQueryCallResult<T> {
    pub fn output_ref(&self) -> &Self {
        self
    }
}

impl<'a, T> Try for &'a NestedQueryCallResult<T> {
    type Output = &'a T;

    type Residual = anyhow::Result<Infallible>;

    fn from_output(output: Self::Output) -> Self {
        // We never actually use this
        unimplemented!()
    }

    fn branch(self) -> std::ops::ControlFlow<Self::Residual, Self::Output> {
        match self.result.output_ref() {
            Ok(x) => std::ops::ControlFlow::Continue(x),
            Err(err) => {
                // FIXME: don't filter this via environment, do it on the client
                if std::env::var_os("CEALN_SHOW_CALL_ERROR_CONTEXT").is_some() {
                    std::ops::ControlFlow::Break(Err(err.context(QueryCallErrorContext {
                        called_query: self.called_query.clone(),
                        calling_query: self.calling_query.clone(),
                    })))
                } else {
                    std::ops::ControlFlow::Break(Err(err))
                }
            }
        }
    }
}
impl<'a, T> FromResidual for &'a NestedQueryCallResult<T> {
    fn from_residual(residual: <Self as Try>::Residual) -> Self {
        // We never actually use this
        unimplemented!()
    }
}
