pub mod grpc {
    pub use crate::grpc::workspace_builder::*;

    pub use self::{workspace_builder_client as client, workspace_builder_server as server};
}

use std::{collections::BTreeMap, convert::TryFrom, ffi::OsString, time::SystemTime};

use prost_types::Timestamp;

use cealn_data::{reference::Reference, rule::BuildConfig, LabelBuf};

use crate::{decode_osstring, encode_osstring, parse_string_option, query, ParseError};

#[derive(Clone, Debug)]
pub struct BuildRequest {
    pub targets: Vec<LabelBuf>,
    pub default_package: Option<LabelBuf>,
    pub build_config: BuildConfig,
    pub keep_going: bool,
    pub watch: bool,
}

#[derive(Clone, Debug)]
pub struct RunRequest {
    pub target: LabelBuf,
    pub executable_name: String,
    pub default_package: Option<LabelBuf>,
    pub build_config: BuildConfig,
}

#[derive(Clone, Debug)]
pub struct AnalyzeRequest {
    pub target: LabelBuf,
    pub default_package: Option<LabelBuf>,
    pub build_config: BuildConfig,
}

#[derive(Clone, Debug)]
pub struct ServerStatus {
    pub server_executable_mtime: SystemTime,
    pub launch_environment_variables: BTreeMap<OsString, OsString>,
}

impl From<BuildRequest> for grpc::BuildRequest {
    fn from(msg: BuildRequest) -> grpc::BuildRequest {
        grpc::BuildRequest {
            targets: msg.targets.into_iter().map(|x| x.into()).collect(),
            default_package: msg.default_package.map(|x| x.into()).unwrap_or_else(|| String::new()),
            build_config: Some(msg.build_config.into()),
            keep_going: msg.keep_going,
            watch: msg.watch,
        }
    }
}

impl TryFrom<grpc::BuildRequest> for BuildRequest {
    type Error = ParseError;

    fn try_from(value: grpc::BuildRequest) -> Result<Self, ParseError> {
        Ok(BuildRequest {
            targets: value.targets.into_iter().map(LabelBuf::new).collect::<Result<_, _>>()?,
            default_package: parse_string_option(value.default_package, LabelBuf::new)?,
            build_config: match value.build_config {
                Some(build_config) => build_config.try_into()?,
                None => return Err(ParseError::MissingField("build_config")),
            },
            keep_going: value.keep_going,
            watch: value.watch,
        })
    }
}

impl From<RunRequest> for grpc::RunRequest {
    fn from(msg: RunRequest) -> grpc::RunRequest {
        grpc::RunRequest {
            target: msg.target.into(),
            executable_name: msg.executable_name,
            default_package: msg.default_package.map(|x| x.into()).unwrap_or_else(|| String::new()),
            build_config: Some(msg.build_config.into()),
        }
    }
}

impl TryFrom<grpc::RunRequest> for RunRequest {
    type Error = ParseError;

    fn try_from(value: grpc::RunRequest) -> Result<Self, ParseError> {
        Ok(RunRequest {
            target: value.target.try_into()?,
            executable_name: value.executable_name,
            default_package: parse_string_option(value.default_package, LabelBuf::new)?,
            build_config: match value.build_config {
                Some(build_config) => build_config.try_into()?,
                None => return Err(ParseError::MissingField("build_config")),
            },
        })
    }
}

impl From<BuildConfig> for grpc::BuildConfig {
    fn from(msg: BuildConfig) -> grpc::BuildConfig {
        grpc::BuildConfig {
            options: msg
                .options
                .into_iter()
                .map(|(k, v)| grpc::BuildConfigOption {
                    key: Some(k.into()),
                    value: Some(v.into()),
                })
                .collect(),
            host_options: msg
                .host_options
                .into_iter()
                .map(|(k, v)| grpc::BuildConfigOption {
                    key: Some(k.into()),
                    value: Some(v.into()),
                })
                .collect(),
        }
    }
}

impl TryFrom<grpc::BuildConfig> for BuildConfig {
    type Error = ParseError;

    fn try_from(value: grpc::BuildConfig) -> Result<Self, ParseError> {
        Ok(BuildConfig {
            options: value
                .options
                .into_iter()
                .map(|option| {
                    let key = Reference::try_from(option.key.unwrap())?;
                    let value = Reference::try_from(option.value.unwrap())?;
                    Ok((key, value))
                })
                .collect::<Result<Vec<(Reference, Reference)>, ParseError>>()?,
            host_options: value
                .host_options
                .into_iter()
                .map(|option| {
                    let key = Reference::try_from(option.key.unwrap())?;
                    let value = Reference::try_from(option.value.unwrap())?;
                    Ok((key, value))
                })
                .collect::<Result<Vec<(Reference, Reference)>, ParseError>>()?,
        })
    }
}

impl From<Reference> for grpc::Reference {
    fn from(value: Reference) -> Self {
        grpc::Reference {
            source_label: value.source_label.into(),
            qualname: value.qualname.into(),
        }
    }
}

impl TryFrom<grpc::Reference> for Reference {
    type Error = ParseError;

    fn try_from(value: grpc::Reference) -> Result<Self, Self::Error> {
        Ok(Reference {
            source_label: value.source_label.try_into()?,
            qualname: value.qualname,
        })
    }
}

impl From<ServerStatus> for grpc::ServerStatus {
    fn from(msg: ServerStatus) -> grpc::ServerStatus {
        grpc::ServerStatus {
            server_executable_mtime: Some(Timestamp::from(msg.server_executable_mtime)),
            launch_environment_variables: msg
                .launch_environment_variables
                .into_iter()
                .map(|(k, v)| grpc::EnvironmentEntry {
                    key: encode_osstring(k).into_owned(),
                    value: encode_osstring(v).into_owned(),
                })
                .collect(),
        }
    }
}

impl TryFrom<grpc::ServerStatus> for ServerStatus {
    type Error = ParseError;

    fn try_from(value: grpc::ServerStatus) -> Result<ServerStatus, ParseError> {
        Ok(ServerStatus {
            server_executable_mtime: SystemTime::try_from(
                value
                    .server_executable_mtime
                    .ok_or(ParseError::MissingField("server_executable_mtime"))?,
            )
            .map_err(|_| ParseError::InvalidTimestamp)?,
            launch_environment_variables: value
                .launch_environment_variables
                .into_iter()
                .map(|entry| {
                    (
                        decode_osstring(entry.key).into_owned(),
                        decode_osstring(entry.value).into_owned(),
                    )
                })
                .collect(),
        })
    }
}
