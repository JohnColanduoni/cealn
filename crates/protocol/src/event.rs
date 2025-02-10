pub mod grpc {
    pub use crate::grpc::event::*;
}

use std::convert::{TryFrom, TryInto};

use cealn_data::{action::StructuredMessageLevel, LabelBuf};
use chrono::{DateTime, Utc};

use crate::{decode_datetime, encode_datetime, file::SystemFilename, query::StdioLine, ParseError};

#[derive(Clone, Debug)]
pub struct BuildEvent {
    pub timestamp: DateTime<Utc>,
    pub source: Option<BuildEventSource>,
    pub data: BuildEventData,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum BuildEventSource {
    RootWorkspaceLoad,
    PackageLoad { label: LabelBuf },
    RuleAnalysis { target_label: LabelBuf },
    Action { mnemonic: String, progress_message: String },
    ActionAnalysis { mnemonic: String, progress_message: String },
    Output { label: LabelBuf },
    InternalQuery,
}

#[derive(Clone, Debug)]
pub enum BuildEventData {
    InternalError(InternalError),
    Stdio {
        line: StdioLine,
    },
    Message {
        level: StructuredMessageLevel,
        data: prost_types::Struct,
        human_message: Option<String>,
    },
    QueryRunStart,
    QueryRunEnd,
    CacheCheckStart,
    CacheCheckEnd,
    Progress {
        fraction: f64,
    },
    WorkspaceFileNotFound {
        directory: SystemFilename,
        exists_with_different_case: bool,
    },
    ExecutablePrepped {
        executable_path: String,
        parent_pid: u32,
    },
    ActionCacheHit,
    WatchRun,
    WatchIdle,
}

#[derive(Clone, Debug)]
pub struct InternalError {
    pub message: String,
    pub backtrace: Vec<String>,
    pub cause: Option<Box<InternalError>>,
    pub nested_query: bool,
}

impl From<BuildEvent> for grpc::BuildEvent {
    fn from(msg: BuildEvent) -> grpc::BuildEvent {
        grpc::BuildEvent {
            timestamp: Some(encode_datetime(&msg.timestamp)),
            source: msg.source.map(|source| {
                use grpc::build_event::Source::*;
                match source {
                    BuildEventSource::RootWorkspaceLoad => RootWorkspaceLoad(grpc::RootWorkspaceLoadSource {}),
                    BuildEventSource::PackageLoad { label } => {
                        PackageLoad(grpc::PackageLoadSource { label: label.into() })
                    }
                    BuildEventSource::RuleAnalysis { target_label } => RuleAnalysis(grpc::RuleAnalysisSource {
                        target_label: target_label.into(),
                    }),
                    BuildEventSource::Action {
                        mnemonic,
                        progress_message,
                    } => Action(grpc::ActionSource {
                        mnemonic,
                        progress_message,
                    }),
                    BuildEventSource::ActionAnalysis {
                        mnemonic,
                        progress_message,
                    } => ActionAnalysis(grpc::ActionAnalysisSource {
                        mnemonic,
                        progress_message,
                    }),
                    BuildEventSource::Output { label } => Output(grpc::OutputSource { label: label.into() }),
                    BuildEventSource::InternalQuery => InternalQuery(grpc::InternalQuerySource {}),
                }
            }),
            data: Some({
                use grpc::build_event::Data::*;
                match msg.data {
                    BuildEventData::InternalError(data) => InternalError(grpc::InternalError::from(data)),
                    BuildEventData::Stdio { line } => Stdio(grpc::Stdio {
                        line: line.contents,
                        stream: match line.stream {
                            crate::query::StdioStreamType::Stdout => grpc::StdioStreamType::StdioStdout.into(),
                            crate::query::StdioStreamType::Stderr => grpc::StdioStreamType::StdioStderr.into(),
                        },
                    }),
                    BuildEventData::Message {
                        level,
                        data,
                        human_message: human_field,
                    } => Message(grpc::StructuredMessage {
                        level: match level {
                            StructuredMessageLevel::Error => grpc::StructuredMessageLevel::LevelError.into(),
                            StructuredMessageLevel::Warn => grpc::StructuredMessageLevel::LevelWarn.into(),
                            StructuredMessageLevel::Info => grpc::StructuredMessageLevel::LevelInfo.into(),
                            StructuredMessageLevel::Debug => grpc::StructuredMessageLevel::LevelDebug.into(),
                        },
                        data: Some(data),
                        human_field: human_field.unwrap_or_default(),
                    }),
                    BuildEventData::QueryRunStart => QueryRunStart(grpc::QueryRunStart {}),
                    BuildEventData::QueryRunEnd => QueryRunEnd(grpc::QueryRunEnd {}),
                    BuildEventData::CacheCheckStart => CacheCheckStart(grpc::CacheCheckStart {}),
                    BuildEventData::CacheCheckEnd => CacheCheckEnd(grpc::CacheCheckEnd {}),
                    BuildEventData::Progress { fraction } => Progress(grpc::Progress { fraction }),
                    BuildEventData::WorkspaceFileNotFound {
                        directory,
                        exists_with_different_case,
                    } => WorkspaceFileNotFound(grpc::WorkspaceFileNotFound {
                        directory: Some(directory.into()),
                        exists_with_different_case,
                    }),
                    BuildEventData::ExecutablePrepped {
                        executable_path,
                        parent_pid,
                    } => ExecutablePrepped(grpc::ExecutablePrepped {
                        executable_path,
                        parent_pid,
                    }),
                    BuildEventData::ActionCacheHit => ActionCacheHit(grpc::ActionCacheHit {}),
                    BuildEventData::WatchRun => WatchRun(grpc::WatchRun {}),
                    BuildEventData::WatchIdle => WatchIdle(grpc::WatchIdle {}),
                }
            }),
        }
    }
}

impl From<InternalError> for grpc::InternalError {
    fn from(value: InternalError) -> Self {
        grpc::InternalError {
            message: value.message,
            backtrace: value.backtrace,
            cause: value.cause.map(|x| Box::new(grpc::InternalError::from(*x))),
            nested_query: value.nested_query,
        }
    }
}

impl TryFrom<grpc::BuildEvent> for BuildEvent {
    type Error = ParseError;

    fn try_from(value: grpc::BuildEvent) -> Result<Self, ParseError> {
        Ok(BuildEvent {
            timestamp: decode_datetime(&value.timestamp.ok_or(ParseError::MissingField("timestamp"))?)?,
            source: {
                use grpc::build_event::Source::*;
                if let Some(source) = value.source {
                    Some(match source {
                        RootWorkspaceLoad(_data) => BuildEventSource::RootWorkspaceLoad,
                        PackageLoad(data) => BuildEventSource::PackageLoad {
                            label: data.label.try_into()?,
                        },
                        RuleAnalysis(data) => BuildEventSource::RuleAnalysis {
                            target_label: data.target_label.try_into()?,
                        },
                        Action(data) => BuildEventSource::Action {
                            mnemonic: data.mnemonic,
                            progress_message: data.progress_message,
                        },
                        ActionAnalysis(data) => BuildEventSource::ActionAnalysis {
                            mnemonic: data.mnemonic,
                            progress_message: data.progress_message,
                        },
                        Output(data) => BuildEventSource::Output {
                            label: data.label.try_into()?,
                        },
                        InternalQuery(_) => BuildEventSource::InternalQuery,
                    })
                } else {
                    None
                }
            },
            data: {
                use grpc::build_event::Data::*;
                match value.data.ok_or(ParseError::MissingField("data"))? {
                    InternalError(data) => BuildEventData::InternalError(self::InternalError::try_from(data)?),
                    Stdio(data) => BuildEventData::Stdio {
                        line: StdioLine {
                            stream: match data.stream {
                                1 => crate::query::StdioStreamType::Stdout,
                                2 => crate::query::StdioStreamType::Stderr,
                                _ => return Err(ParseError::UnknownEnumValue("StdioStreamType")),
                            },
                            contents: data.line,
                        },
                    },
                    Message(message) => BuildEventData::Message {
                        level: match message.level {
                            1 => StructuredMessageLevel::Error,
                            2 => StructuredMessageLevel::Warn,
                            3 => StructuredMessageLevel::Info,
                            4 => StructuredMessageLevel::Debug,
                            _ => return Err(ParseError::UnknownEnumValue("StructuredMessageLevel")),
                        },
                        data: message.data.ok_or(ParseError::MissingField("data"))?,
                        human_message: if !message.human_field.is_empty() {
                            Some(message.human_field)
                        } else {
                            None
                        },
                    },
                    QueryRunStart(_) => BuildEventData::QueryRunStart,
                    QueryRunEnd(_) => BuildEventData::QueryRunEnd,
                    CacheCheckStart(_) => BuildEventData::CacheCheckStart,
                    CacheCheckEnd(_) => BuildEventData::CacheCheckEnd,
                    Progress(data) => BuildEventData::Progress {
                        fraction: data.fraction,
                    },
                    WorkspaceFileNotFound(data) => BuildEventData::WorkspaceFileNotFound {
                        directory: data
                            .directory
                            .ok_or(ParseError::MissingField("directory"))?
                            .try_into()?,
                        exists_with_different_case: data.exists_with_different_case,
                    },
                    ExecutablePrepped(data) => BuildEventData::ExecutablePrepped {
                        executable_path: data.executable_path,
                        parent_pid: data.parent_pid,
                    },
                    ActionCacheHit(_) => BuildEventData::ActionCacheHit,
                    WatchRun(_) => BuildEventData::WatchRun,
                    WatchIdle(_) => BuildEventData::WatchIdle,
                }
            },
        })
    }
}

impl TryFrom<grpc::InternalError> for InternalError {
    type Error = ParseError;

    fn try_from(value: grpc::InternalError) -> Result<Self, Self::Error> {
        let cause = match value.cause {
            Some(cause) => Some(Box::new(InternalError::try_from(*cause)?)),
            None => None,
        };
        Ok(InternalError {
            message: value.message,
            backtrace: value.backtrace,
            cause,
            nested_query: value.nested_query,
        })
    }
}
