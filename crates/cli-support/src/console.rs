use std::{
    cmp,
    collections::HashSet,
    io::{BufWriter, Stderr, Write},
};

use cealn_client::{
    BuildEvent, BuildEventData, BuildEventSource, InternalError, StdioStreamType, StructuredMessageLevel,
};
use crossterm::{
    cursor, style,
    terminal::{self, ClearType},
    ExecutableCommand, QueueableCommand,
};

pub struct Console {
    options: ConsoleOptions,
    longest_source_description: usize,

    running_action_sources: Vec<BuildEventSource>,
    running_sources: Vec<BuildEventSource>,
    cache_checking_sources: Vec<BuildEventSource>,
    cache_hit_sources: HashSet<BuildEventSource>,
    last_stdio_source: Option<BuildEventSource>,

    running_compose_sources: Vec<ComposeEventSource>,
    last_compose_output_source: Option<ComposeEventSource>,
}

const ERROR_COLOR: style::Color = style::Color::Red;

pub struct ConsoleOptions {
    pub tty: bool,
    pub print_cached_output: bool,
    pub max_level: Option<StructuredMessageLevel>,
}

#[derive(Clone, Debug)]
pub struct ComposeEvent {
    pub source: Option<ComposeEventSource>,
    pub data: ComposeEventData,
}

#[derive(Clone, Debug)]
pub enum ComposeEventData {
    Start,
    End,
    NewObject,
    ModifyObjectField {
        field_path: String,
        old_value: serde_json::Value,
        new_value: serde_json::Value,
    },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ComposeEventSource {
    Deployment,
    Apply {
        kind: String,
        name: String,
        namespace: Option<String>,
    },
    Push {
        image_name: String,
        full_image_name: String,
        tag: String,
    },
    PushLayer {
        image_name: String,
        full_image_name: String,
        tag: String,
        digest: String,
    },
    DockerCredentialsFetch {
        registry: String,
    },
    VolumeSync {
        namespace: String,
        persistent_volume_claim: String,
    },
}

impl Console {
    pub fn new(options: ConsoleOptions) -> Console {
        let console = Console {
            options,
            longest_source_description: 0,
            running_action_sources: Default::default(),
            running_sources: Default::default(),
            cache_checking_sources: Default::default(),
            cache_hit_sources: Default::default(),
            last_stdio_source: None,
            running_compose_sources: Default::default(),
            last_compose_output_source: None,
        };

        if console.options.tty {
            let mut stderr = std::io::stderr();
            stderr.execute(cursor::SavePosition).unwrap();
        }

        console
    }

    pub fn push_build_event(&mut self, event: &BuildEvent) {
        if self.options.tty {
            self.push_build_event_tty(event);
        } else {
            self.push_build_event_notty(event);
        }
    }

    pub fn push_compose_event(&mut self, event: &ComposeEvent) {
        if self.options.tty {
            self.push_compose_event_tty(event);
        } else {
            todo!()
        }
    }

    pub fn clear(&mut self) {
        if self.options.tty {
            let mut stderr = BufWriter::with_capacity(32 * 1024, std::io::stderr());
            stderr.queue(terminal::Clear(ClearType::Purge)).unwrap();
            stderr.queue(cursor::MoveTo(0, 0)).unwrap();
            stderr.queue(cursor::SavePosition).unwrap();
            stderr.flush().unwrap();
        }
    }

    pub fn scroll_to_top(&mut self) {
        if self.options.tty {
            let mut stderr = BufWriter::with_capacity(32 * 1024, std::io::stderr());
            // FIXME: doesn't work
            // stderr.queue(terminal::ScrollUp(u16::MAX)).unwrap();
            stderr.flush().unwrap();
        }
    }

    fn push_build_event_tty(&mut self, event: &BuildEvent) {
        let mut update_sources = false;

        let mut stderr = BufWriter::with_capacity(32 * 1024, std::io::stderr());

        match &event.data {
            BuildEventData::InternalError(InternalError {
                message,
                backtrace,
                cause,
                nested_query,
            }) => {
                stderr.queue(cursor::RestorePosition).unwrap();
                stderr.queue(terminal::Clear(ClearType::FromCursorDown)).unwrap();
                if !(backtrace.is_empty() && *nested_query) {
                    writeln!(&mut stderr, "{}", message).unwrap();
                    for frame in backtrace {
                        writeln!(&mut stderr, "{}", frame).unwrap();
                    }
                }
                let mut next_cause = cause.as_deref();
                while let Some(cause) = next_cause {
                    if cause.backtrace.is_empty() && cause.nested_query {
                        next_cause = cause.cause.as_deref();
                        continue;
                    }
                    writeln!(&mut stderr, "{}", cause.message).unwrap();
                    for frame in &cause.backtrace {
                        writeln!(&mut stderr, "{}", frame).unwrap();
                    }
                    next_cause = cause.cause.as_deref()
                }
                stderr.queue(cursor::SavePosition).unwrap();
                update_sources = true;
            }
            BuildEventData::Stdio { line } => {
                if !self.options.print_cached_output
                    && event
                        .source
                        .as_ref()
                        .map(|source| self.cache_hit_sources.contains(source))
                        == Some(true)
                {
                    return;
                }

                stderr.queue(cursor::RestorePosition).unwrap();
                stderr.queue(terminal::Clear(ClearType::FromCursorDown)).unwrap();
                if let Some(source) = &event.source {
                    if self.last_stdio_source.as_ref().map(|x| x == source) != Some(true) {
                        let mnemonic_color = match line.stream {
                            StdioStreamType::Stdout => style::Color::DarkGreen,
                            StdioStreamType::Stderr => style::Color::Red,
                        };
                        self.describe_source(&mut stderr, source, mnemonic_color);
                        writeln!(&mut stderr, "").unwrap();
                        self.last_stdio_source = Some(source.clone());
                    }
                }
                writeln!(&mut stderr, "{}", String::from_utf8_lossy(&line.contents)).unwrap();
                stderr.queue(cursor::SavePosition).unwrap();
                update_sources = true;
            }
            BuildEventData::Message {
                level,
                data,
                human_message,
            } => {
                if !self.options.print_cached_output
                    && event
                        .source
                        .as_ref()
                        .map(|source| self.cache_hit_sources.contains(source))
                        == Some(true)
                {
                    return;
                }

                if let Some(message) = human_message.as_ref() && self.options.max_level.map(|max_level| level <= &max_level).unwrap_or(true) {
                    stderr.queue(cursor::RestorePosition).unwrap();
                    stderr.queue(terminal::Clear(ClearType::FromCursorDown)).unwrap();
                    if let Some(source) = &event.source {
                        if self.last_stdio_source.as_ref().map(|x| x == source) != Some(true) {
                            let mnemonic_color = match level {
                                StructuredMessageLevel::Error => style::Color::Red,
                                StructuredMessageLevel::Warn => style::Color::Yellow,
                                _ => style::Color::Grey,
                            };
                            self.describe_source(&mut stderr, source, mnemonic_color);
                            writeln!(&mut stderr, "").unwrap();
                            self.last_stdio_source = Some(source.clone());
                        }
                    }
                    write!(&mut stderr, "{}", message).unwrap();
                    if !message.ends_with('\n') {
                        writeln!(&mut stderr, "").unwrap();
                    }
                    stderr.queue(cursor::SavePosition).unwrap();
                    update_sources = true;
                }
            }
            BuildEventData::QueryRunStart => {
                if let Some(source) = event.source.clone() {
                    match source {
                        BuildEventSource::Action { ref mnemonic, .. } if mnemonic == "BuildDepmap" => {}
                        BuildEventSource::ActionAnalysis { ref mnemonic, .. } if mnemonic == "BuildDepmap" => {}
                        BuildEventSource::Output { .. } => {}
                        BuildEventSource::InternalQuery => {}
                        BuildEventSource::ActionAnalysis { .. } => {}
                        BuildEventSource::Action { ref mnemonic, .. } => {
                            self.running_action_sources.push(source);
                            update_sources = true;
                        }
                        source => {
                            self.running_sources.push(source);
                            update_sources = true;
                        }
                    }
                }
            }
            BuildEventData::QueryRunEnd => {
                if let Some(source) = &event.source {
                    if let Some(index) = self.running_sources.iter().position(|x| x == source) {
                        self.running_sources.remove(index);
                        update_sources = true;
                    }
                    if let Some(index) = self.running_action_sources.iter().position(|x| x == source) {
                        self.running_action_sources.remove(index);
                        update_sources = true;
                    }
                    self.cache_hit_sources.remove(source);
                }
            }
            BuildEventData::CacheCheckStart => {
                if let Some(source) = event.source.clone() {
                    match source {
                        BuildEventSource::Action { ref mnemonic, .. } if mnemonic == "BuildDepmap" => {}
                        BuildEventSource::ActionAnalysis { ref mnemonic, .. } if mnemonic == "BuildDepmap" => {}
                        BuildEventSource::Output { .. } => {}
                        BuildEventSource::InternalQuery => {}
                        BuildEventSource::ActionAnalysis { .. } => {}
                        source => {
                            self.cache_checking_sources.push(source);
                            update_sources = true;
                        }
                    }
                }
            }
            BuildEventData::CacheCheckEnd => {
                if let Some(source) = &event.source {
                    if let Some(index) = self.cache_checking_sources.iter().position(|x| x == source) {
                        self.cache_checking_sources.remove(index);
                        update_sources = true;
                    }
                }
            }
            BuildEventData::Progress { fraction } => {
                // FIXME: show
            }
            BuildEventData::WorkspaceFileNotFound {
                directory,
                exists_with_different_case,
            } => todo!(),
            BuildEventData::ExecutablePrepped { .. } => {}
            BuildEventData::ActionCacheHit { .. } => {
                if let Some(source) = event.source.clone() {
                    self.cache_hit_sources.insert(source);
                }
            }
            BuildEventData::WatchRun => {}
            BuildEventData::WatchIdle => {}
        }

        if update_sources {
            self.update_sources(&mut stderr);
        }
        stderr.flush().unwrap();
    }

    fn update_sources(&mut self, stderr: &mut BufWriter<Stderr>) {
        stderr.queue(cursor::RestorePosition).unwrap();
        stderr.queue(cursor::Hide).unwrap();
        stderr.queue(terminal::DisableLineWrap).unwrap();
        let (_, height) = terminal::size().unwrap();
        let mut printed_sources = 0;
        let mut overflow = false;
        for source in self.running_compose_sources.clone().into_iter() {
            if printed_sources + 5 > height as usize {
                overflow = true;
                break;
            }
            stderr.queue(terminal::Clear(ClearType::CurrentLine)).unwrap();
            self.describe_compose_source(stderr, &source, style::Color::Green);
            writeln!(stderr, "").unwrap();
            printed_sources += 1;
        }
        for source in self.running_action_sources.clone().into_iter() {
            if printed_sources + 5 > height as usize {
                overflow = true;
                break;
            }
            stderr.queue(terminal::Clear(ClearType::CurrentLine)).unwrap();
            self.describe_source(stderr, &source, style::Color::Green);
            writeln!(stderr, "").unwrap();
            printed_sources += 1;
        }
        for source in self.running_sources.clone().into_iter() {
            if printed_sources + 5 > height as usize {
                overflow = true;
                break;
            }
            stderr.queue(terminal::Clear(ClearType::CurrentLine)).unwrap();
            self.describe_source(stderr, &source, style::Color::Green);
            writeln!(stderr, "").unwrap();
            printed_sources += 1;
        }
        for source in self.cache_checking_sources.clone().into_iter() {
            if printed_sources + 5 > height as usize {
                overflow = true;
                break;
            }
            stderr.queue(terminal::Clear(ClearType::CurrentLine)).unwrap();
            self.describe_source(stderr, &source, style::Color::Blue);
            writeln!(stderr, "").unwrap();
            printed_sources += 1;
        }
        stderr.queue(terminal::Clear(ClearType::FromCursorDown)).unwrap();
        if overflow {
            writeln!(
                stderr,
                "{} actions, {} queries",
                self.running_action_sources.len(),
                self.running_sources.len()
            )
            .unwrap();
        }
        stderr.queue(terminal::EnableLineWrap).unwrap();
        stderr.queue(cursor::Show).unwrap();
    }

    fn push_compose_event_tty(&mut self, event: &ComposeEvent) {
        let mut update_sources = false;

        let mut stderr = BufWriter::with_capacity(32 * 1024, std::io::stderr());

        match &event.data {
            ComposeEventData::Start => {
                if let Some(source) = event.source.clone() {
                    self.running_compose_sources.push(source);
                    update_sources = true;
                }
            }
            ComposeEventData::End => {
                if let Some(source) = &event.source {
                    if let Some(index) = self.running_compose_sources.iter().position(|x| x == source) {
                        self.running_compose_sources.remove(index);
                        update_sources = true;
                    }
                }
            }
            ComposeEventData::NewObject => {}
            ComposeEventData::ModifyObjectField {
                field_path,
                old_value,
                new_value,
            } => {
                stderr.queue(cursor::RestorePosition).unwrap();
                stderr.queue(terminal::Clear(ClearType::FromCursorDown)).unwrap();
                if let Some(source) = &event.source {
                    if self.last_compose_output_source.as_ref().map(|x| x == source) != Some(true) {
                        self.describe_compose_source(&mut stderr, source, style::Color::Grey);
                        writeln!(&mut stderr, "").unwrap();
                        self.last_compose_output_source = Some(source.clone());
                    }
                }
                writeln!(
                    &mut stderr,
                    "updated field {}: {}",
                    field_path,
                    serde_json::to_string(new_value).unwrap()
                )
                .unwrap();
                stderr.queue(cursor::SavePosition).unwrap();
                update_sources = true;
            }
        }

        if update_sources {
            self.update_sources(&mut stderr);
        }
        stderr.flush().unwrap();
    }

    fn describe_source<W>(&mut self, output: &mut W, source: &BuildEventSource, mnemonic_color: style::Color)
    where
        W: Write,
    {
        let source_description = match source {
            BuildEventSource::RootWorkspaceLoad => format!("LoadWorkspace"),
            BuildEventSource::PackageLoad { .. } => format!("LoadPackage"),
            BuildEventSource::RuleAnalysis { .. } => format!("Analyze"),
            BuildEventSource::Action { mnemonic, .. } => format!("{}", mnemonic),
            BuildEventSource::ActionAnalysis { mnemonic, .. } => format!("{}", mnemonic),
            BuildEventSource::Output { .. } => format!("Output"),
            BuildEventSource::InternalQuery => format!("Internal"),
        };

        self.longest_source_description = cmp::max(self.longest_source_description, source_description.len());

        let padding = self.longest_source_description - source_description.len();
        for _ in 0..padding {
            write!(output, " ").unwrap();
        }
        output.queue(style::SetForegroundColor(mnemonic_color)).unwrap();
        write!(output, "{}", source_description).unwrap();
        output.queue(style::ResetColor).unwrap();

        match source {
            BuildEventSource::PackageLoad { label } => write!(output, " {}", label).unwrap(),
            BuildEventSource::RuleAnalysis { target_label } => write!(output, " {}", target_label).unwrap(),
            BuildEventSource::Output { label } => write!(output, " {}", label).unwrap(),
            BuildEventSource::Action { progress_message, .. } => write!(output, " {}", progress_message).unwrap(),
            BuildEventSource::ActionAnalysis { progress_message, .. } => {
                write!(output, " {}", progress_message).unwrap()
            }
            _ => {}
        }
    }

    fn describe_compose_source<W>(&mut self, output: &mut W, source: &ComposeEventSource, mnemonic_color: style::Color)
    where
        W: Write,
    {
        let source_description = match source {
            ComposeEventSource::Deployment => format!("ComposeDeploy"),
            ComposeEventSource::Apply { .. } => format!("ComposeApply"),
            ComposeEventSource::Push { .. } => format!("ComposePush"),
            ComposeEventSource::PushLayer { .. } => format!("ComposePushLayer"),
            ComposeEventSource::DockerCredentialsFetch { .. } => format!("ComposeDockerCredentials"),
            ComposeEventSource::VolumeSync { .. } => format!("ComposeVolumeSync"),
        };

        self.longest_source_description = cmp::max(self.longest_source_description, source_description.len());

        let padding = self.longest_source_description - source_description.len();
        for _ in 0..padding {
            write!(output, " ").unwrap();
        }
        output.queue(style::SetForegroundColor(mnemonic_color)).unwrap();
        write!(output, "{}", source_description).unwrap();
        output.queue(style::ResetColor).unwrap();

        match source {
            ComposeEventSource::Deployment => {}
            ComposeEventSource::Apply {
                kind,
                namespace: None,
                name,
            } => write!(output, " {: >24} {}", kind, name).unwrap(),
            ComposeEventSource::Apply {
                kind,
                namespace: Some(namespace),
                name,
            } => write!(output, " {: >24} {}/{}", kind, namespace, name).unwrap(),
            ComposeEventSource::Push {
                image_name,
                full_image_name,
                tag,
            } => write!(output, " {} -> {}:{}", image_name, full_image_name, tag).unwrap(),
            ComposeEventSource::PushLayer {
                image_name,
                full_image_name,
                tag,
                digest,
            } => write!(output, " {} -> {}:{} {}", image_name, full_image_name, tag, digest).unwrap(),
            ComposeEventSource::DockerCredentialsFetch { registry } => write!(output, " {}", registry).unwrap(),
            ComposeEventSource::VolumeSync {
                namespace,
                persistent_volume_claim,
            } => write!(output, " {} {}", namespace, persistent_volume_claim).unwrap(),
        }
    }

    fn push_build_event_notty(&mut self, event: &BuildEvent) {
        let source_type_description = match &event.source {
            Some(BuildEventSource::RootWorkspaceLoad) => "[workspace]",
            Some(BuildEventSource::PackageLoad { label }) => "[     load]",
            Some(BuildEventSource::RuleAnalysis { target_label }) => "[  analyze]",
            Some(BuildEventSource::Action { mnemonic, .. }) => {
                if mnemonic == "BuildDepmap" {
                    return;
                }
                "[   action]"
            }
            Some(BuildEventSource::ActionAnalysis { mnemonic, .. }) => {
                return;
            }
            Some(BuildEventSource::Output { .. }) => "[   output]",
            Some(BuildEventSource::InternalQuery) => return,
            None => "[         ]",
        };

        let source_description = match &event.source {
            Some(BuildEventSource::RootWorkspaceLoad) => format!(""),
            Some(BuildEventSource::PackageLoad { label }) => format!("{}", label),
            Some(BuildEventSource::RuleAnalysis { target_label }) => format!("{}", target_label),
            Some(BuildEventSource::Action { mnemonic, .. }) => format!("{}", mnemonic),
            Some(BuildEventSource::ActionAnalysis { mnemonic, .. }) => format!("{}", mnemonic),
            Some(BuildEventSource::Output { label }) => format!("{}", label),
            Some(BuildEventSource::InternalQuery) => return,
            None => "".to_owned(),
        };

        self.longest_source_description = cmp::max(self.longest_source_description, source_description.len());

        eprint!("{}[", source_type_description);
        let padding = self.longest_source_description - source_description.len();
        for _ in 0..padding {
            eprint!(" ");
        }
        eprint!("{}] ", source_description);

        match &event.data {
            BuildEventData::InternalError(InternalError {
                message,
                backtrace,
                cause,
                ..
            }) => {
                eprintln!("{}", message);
                for frame in backtrace {
                    eprintln!("{}", frame);
                }
                let mut next_cause = cause.as_deref();
                while let Some(cause) = next_cause {
                    eprintln!("{}", cause.message);
                    for frame in &cause.backtrace {
                        eprintln!("{}", frame);
                    }
                    next_cause = cause.cause.as_deref()
                }
            }
            BuildEventData::Stdio { line } => {
                eprintln!("{}", String::from_utf8_lossy(&line.contents));
            }
            BuildEventData::Message {
                level,
                data,
                human_message: human_field,
            } => {
                // FIXME
            }
            BuildEventData::QueryRunStart => match &event.source {
                Some(BuildEventSource::Action { progress_message, .. }) => eprintln!("{}", progress_message),
                _ => eprintln!("start"),
            },
            BuildEventData::QueryRunEnd => {
                eprintln!("end")
            }
            BuildEventData::CacheCheckStart => match &event.source {
                Some(BuildEventSource::Action { progress_message, .. }) => eprintln!("{}", progress_message),
                _ => eprintln!("cache check start"),
            },
            BuildEventData::CacheCheckEnd => {
                eprintln!("cache check end")
            }
            BuildEventData::Progress { fraction } => {
                eprintln!("{:.1}%", fraction * 100.0);
            }
            BuildEventData::WorkspaceFileNotFound {
                directory,
                exists_with_different_case,
            } => todo!(),
            BuildEventData::ExecutablePrepped { .. } => {}
            BuildEventData::ActionCacheHit => {}
            BuildEventData::WatchRun => {}
            BuildEventData::WatchIdle => {}
        }
    }
}
