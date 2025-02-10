use std::{
    collections::{BTreeMap, HashMap},
    io::{BufRead, BufReader},
    path::Path,
    sync::Arc,
};

use cealn_cache::hot_disk;
use cealn_event::{BuildEventData, EventContext};
use futures::{prelude::*, stream::FuturesUnordered};

use anyhow::{anyhow, Context};
use cealn_data::{
    action::{Action, ActionData, ActionOutput, ConcreteAction, LabelAction, StructuredMessageConfig},
    cache::Cacheability,
    depmap::ConcreteDepmapReference,
    rule::BuildConfig,
    Label, LabelBuf,
};
use cealn_protocol::{
    event::BuildEventSource,
    query::{AnalysisQuery, OutputQuery, QueryType, StdioStreamType},
};
use tracing::info_span;

use crate::graph::graph::{GraphQueryInternal, _Graph};

use super::{QueryNode, QueryRunnerDriver};

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ActionQuery {
    pub action: LabelAction,
    pub build_config: BuildConfig,
    // Used when an analysis pass requests an action it constructed itself as a dependency. This prevents a circular
    // dependency on the analysis to itself.
    pub partial_actions: BTreeMap<LabelBuf, LabelAction>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ConcreteActionQuery {
    pub action: ConcreteAction,
}

impl QueryType for ActionQuery {
    type Product = ActionOutput;

    const KIND: &'static str = "action";
}

impl GraphQueryInternal for ActionQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<ActionOutput>> + Send + 'a;

    fn get_query_node(graph: &_Graph, query: Self) -> Arc<QueryNode<Self>> {
        graph.get_or_create_query(query, &graph.action_queries)
    }

    fn run<'a>(runner: &'a mut QueryRunnerDriver<Self>) -> Self::Run<'a> {
        async move {
            // Transitions are handled specially; they don't have a concrete version, instead they trigger another action query
            if let ActionData::Transition(transition) = &runner.shared.query.action.data {
                let mut patched_build_config = runner.shared.query.build_config.clone();
                for (k, v) in &transition.changed_options {
                    patched_build_config.options.retain(|(old_k, old_v)| old_k != k);
                    patched_build_config.options.push((k.clone(), v.clone()));
                }
                let result = runner
                    .query(OutputQuery {
                        target_label: transition.label.clone(),
                        build_config: patched_build_config,
                    })
                    .await;
                let output = result.output_ref()?;

                let reference = output
                    .reference
                    .as_ref()
                    .context("missing output from transition source")?;

                let files = match reference.subpath {
                    Some(_) => todo!(),
                    None => reference.hash,
                };

                return Ok(ActionOutput {
                    files,
                    stdout: None,
                    stderr: None,
                });
            }

            // Resolve source depmap to concrete files. This entails either pulling them from a cache or building them
            // outright.
            let mut concrete_depmap_mappings = HashMap::new();
            {
                let mut querying_futures = FuturesUnordered::new();
                for (source_depmap, build_config_override) in runner
                    .shared
                    .query
                    .action
                    .source_depmaps(&runner.shared.query.build_config)
                {
                    querying_futures.push({
                        let runner = &*runner;
                        async move {
                            let parts = source_depmap.parts();
                            if let Some(action_id) = parts.action_id {
                                let action_label = parts.full_action().unwrap();
                                // First check partial actions from query
                                let action_query_output;
                                let output_reference = if let Some(partial_action) =
                                    runner.shared.query.partial_actions.get(action_label)
                                {
                                    action_query_output = runner
                                        .query(ActionQuery {
                                            action: partial_action.clone(),
                                            partial_actions: runner.shared.query.partial_actions.clone(),
                                            build_config: build_config_override
                                                .unwrap_or_else(|| runner.shared.query.build_config.clone()),
                                        })
                                        .await;
                                    let action_output = action_query_output.output_ref()?;

                                    let action_path = match parts.action_path {
                                        Some(action_path) => Some(
                                            action_path
                                                .normalize_require_descending()
                                                .context("action path escaped root")?
                                                .into_owned(),
                                        ),
                                        None => None,
                                    };

                                    ConcreteDepmapReference {
                                        hash: action_output.files.clone(),
                                        subpath: action_path,
                                    }
                                } else {
                                    let output = runner
                                        .query(OutputQuery {
                                            target_label: source_depmap.to_owned(),
                                            build_config: build_config_override
                                                .clone()
                                                .unwrap_or_else(|| runner.shared.query.build_config.clone()),
                                        })
                                        .await;

                                    output
                                        .output_ref()?
                                        .reference
                                        .clone()
                                        .context("expected output from query")?
                                };

                                Ok::<(LabelBuf, ConcreteDepmapReference), anyhow::Error>((
                                    source_depmap.to_owned(),
                                    output_reference,
                                ))
                            } else {
                                // Source file
                                let reference = runner
                                    .reference_source_file_as_depmap(source_depmap)
                                    .await?
                                    .ok_or_else(|| {
                                        anyhow!("attempmted to reference non-existent source file {}", source_depmap)
                                    })?;
                                Ok::<(LabelBuf, ConcreteDepmapReference), anyhow::Error>((
                                    source_depmap.to_owned(),
                                    reference,
                                ))
                            }
                        }
                    });
                }

                while let Some((k, v)) = querying_futures.try_next().await? {
                    concrete_depmap_mappings.insert(k, v);
                }
            }

            let concrete_action = runner.shared.query.action.make_concrete(&concrete_depmap_mappings);

            let output = runner
                .query(ConcreteActionQuery {
                    action: concrete_action,
                })
                .await;
            let output = output.output_ref()?;

            Ok(output.clone())
        }
    }

    fn as_event_source(&self) -> BuildEventSource {
        BuildEventSource::ActionAnalysis {
            mnemonic: self.action.mnemonic.clone(),
            progress_message: self.action.progress_message.clone(),
        }
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!(
            "query",
            kind = "action",
            mnemonic = self.action.mnemonic,
            progress_message = self.action.progress_message
        )
    }
}

impl QueryType for ConcreteActionQuery {
    type Product = ActionOutput;

    const KIND: &'static str = "concrete-action";
}

impl GraphQueryInternal for ConcreteActionQuery {
    type Run<'a> = impl Future<Output = anyhow::Result<ActionOutput>> + Send + 'a;

    fn get_query_node(graph: &_Graph, query: Self) -> Arc<QueryNode<Self>> {
        graph.get_or_create_query(query, &graph.concrete_action_queries)
    }

    fn run<'a>(runner: &'a mut QueryRunnerDriver<Self>) -> Self::Run<'a> {
        async move {
            if let Some(output) = runner.graph.lookup_action_cache(&runner.shared.query.action).await? {
                // Validate that the depmaps the action refers to are available, otherwise this is still a cache miss
                // FIXME: check stdout and stderr too
                match runner.graph.lookup_filetree_cache(&output.files).await? {
                    Some(_) => {
                        let mut missing_stdio = false;
                        let mut stdout_guard = None;
                        let mut stderr_guard = None;
                        let mut structured_messages = None;
                        if let ActionData::Run(run) = &runner.shared.query.action.data {
                            structured_messages = run.structured_messages.clone();
                            if let Some(stdout) = &output.stdout && !run.hide_stdout {
                                if let Some(stdout) = runner.graph.open_cache_file(stdout.as_ref(), false).await? {
                                    stdout_guard = Some(stdout);
                                } else {
                                    missing_stdio = true;
                                }
                            }
                            if let Some(stderr) = &output.stderr && !run.hide_stderr {
                                if let Some(stderr) = runner.graph.open_cache_file(stderr.as_ref(), false).await? {
                                    stderr_guard = Some(stderr);
                                } else {
                                    missing_stdio = true;
                                }
                            }
                        }
                        if !missing_stdio {
                            runner.events().send(BuildEventData::ActionCacheHit);

                            if let Some(stdout_guard) = stdout_guard {
                                repeat_stdio(
                                    &stdout_guard,
                                    &mut runner.events_live.fork(),
                                    StdioStreamType::Stdout,
                                    structured_messages.as_ref(),
                                )
                                .await?;
                            }
                            if let Some(stderr_guard) = stderr_guard {
                                repeat_stdio(
                                    &stderr_guard,
                                    &mut runner.events_live.fork(),
                                    StdioStreamType::Stderr,
                                    structured_messages.as_ref(),
                                )
                                .await?;
                            }

                            return Ok(output);
                        }
                    }
                    None => {
                        // Fallthrough to running action
                    }
                }
            }

            match &runner.shared.query.action.data {
                ActionData::Run(_) => {
                    // Don't send QueryRunStart, the run implementation will do it once it actually acquires a process
                    // ticket and start running.
                    runner.sent_start_event = true;
                }
                _ => {
                    runner.events().send(BuildEventData::QueryRunStart);
                    runner.sent_start_event = true;
                }
            }

            let context = runner.get_action_context();
            let result = cealn_action::run(&context, &runner.shared.query.action).await;

            let output = result?;

            match runner.shared.query.action.inherent_cacheability() {
                Cacheability::Uncacheable => {}
                // FIXME: handle global/private
                _ => {
                    runner
                        .graph
                        .write_action_cache(&runner.shared.query.action, &output)
                        .await?
                }
            }

            Ok(output)
        }
    }

    fn as_event_source(&self) -> BuildEventSource {
        BuildEventSource::Action {
            mnemonic: self.action.mnemonic.clone(),
            progress_message: self.action.progress_message.clone(),
        }
    }

    fn construct_span(&self) -> tracing::Span {
        info_span!(
            "query",
            kind = "concrete-action",
            mnemonic = self.action.mnemonic,
            progress_message = self.action.progress_message
        )
    }
}

async fn repeat_stdio(
    file_guard: &Path,
    events: &mut EventContext,
    stream: StdioStreamType,
    structured_messages: Option<&StructuredMessageConfig>,
) -> anyhow::Result<()> {
    // FIXME: async io
    let mut file = BufReader::new(std::fs::File::open(&file_guard)?);
    let mut buffer = Vec::new();
    while file.read_until(b'\n', &mut buffer)? != 0 {
        if buffer.last().cloned() == Some(b'\n') {
            buffer.pop();
        }
        cealn_action_executable::stdio::emit_events_for_line(events, stream, structured_messages, &buffer);
        buffer.clear();
    }
    Ok(())
}
