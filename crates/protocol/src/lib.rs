#![feature(try_trait_v2)]

pub mod event;
pub mod file;
pub mod package;
pub mod query;
pub mod rule;
pub mod workspace;
pub mod workspace_builder;

mod grpc {
    pub mod event {
        tonic::include_proto!("cealn.event");
    }
    pub mod file {
        tonic::include_proto!("cealn.file");
    }
    pub mod workspace_builder {
        tonic::include_proto!("cealn.workspace_builder");
    }
}

use std::{
    borrow::Cow,
    ffi::{OsStr, OsString},
    fs, io,
    path::{Path, PathBuf},
};

use chrono::{DateTime, NaiveDateTime, Utc};
use prost_types::Timestamp;
use thiserror::Error;

use cealn_core::trace_call_result;
use cealn_data::label;

#[derive(Clone, Debug)]
pub struct ServerContext {
    pub workspace_root: PathBuf,
    pub canonical_workspace_root: PathBuf,
    pub build_root: PathBuf,
}

impl ServerContext {
    pub fn new(workspace_root: &Path, build_root: &Path) -> Result<Self, io::Error> {
        // We want to canonicalize the workspace root for actually filesystem access, but the original workspace root
        // provides better paths in error messaging.
        let canonical_workspace_root = trace_call_result!(fs::canonicalize(workspace_root))?;
        trace_call_result!(fs::create_dir_all(build_root))?;
        let build_root = trace_call_result!(fs::canonicalize(build_root))?;

        Ok(ServerContext {
            workspace_root: workspace_root.to_owned(),
            canonical_workspace_root,
            build_root,
        })
    }

    pub fn pid_file_path(&self) -> PathBuf {
        self.build_root.join("server.pid")
    }

    pub fn lock_file_path(&self) -> PathBuf {
        self.build_root.join("server.lock")
    }

    pub fn api_url_file_path(&self) -> PathBuf {
        self.build_root.join("server.api")
    }
}

/// Parses optional strings from protobuf, where they are represented by empty strings
fn parse_string_option<T, E, F: FnOnce(String) -> Result<T, E>>(value: String, f: F) -> Result<Option<T>, E> {
    if !value.is_empty() {
        Ok(Some(f(value)?))
    } else {
        Ok(None)
    }
}

// `OsString`s are encoded using bytes on Unix, and WTF-8 on Windows
fn encode_osstring<'a>(input: impl Into<Cow<'a, OsStr>>) -> Cow<'a, [u8]> {
    let input = input.into();
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            use std::os::unix::prelude::*;

            match input {
                Cow::Borrowed(s) => Cow::Borrowed(s.as_bytes()),
                Cow::Owned(s) => Cow::Owned(s.into_vec()),
            }
        } else if #[cfg(target_os = "windows")] {
            match input {
                Cow::Borrowed(s) => {
                    if let Some(utf8) = s.to_str() {
                        Cow::Borrowed(utf8.as_bytes())
                    } else {
                        unimplemented!();
                    }
                },
                Cow::Owned(s) => {
                    match s.into_string() {
                        Ok(utf8) => Cow::Owned(utf8.into_bytes()),
                        Err(_) => unimplemented!(),
                    }
                }
            }
        } else {
            compile_error!("not implemented for platform");
        }
    }
}

fn decode_osstring<'a>(input: impl Into<Cow<'a, [u8]>>) -> Cow<'a, OsStr> {
    let input = input.into();
    cfg_if::cfg_if! {
        if #[cfg(unix)] {
            use std::os::unix::prelude::*;

            match input {
                Cow::Borrowed(s) => Cow::Borrowed(OsStr::from_bytes(s)),
                Cow::Owned(s) => Cow::Owned(OsString::from_vec(s)),
            }
        } else if #[cfg(target_os = "windows")] {
            match input {
                Cow::Borrowed(s) => {
                    if let Ok(utf8) = std::str::from_utf8(s) {
                        Cow::Borrowed(OsStr::new(utf8))
                    } else {
                        unimplemented!();
                    }
                },
                Cow::Owned(s) => {
                    match String::from_utf8(s) {
                        Ok(utf8) => Cow::Owned(OsString::from(utf8)),
                        Err(_) => unimplemented!(),
                    }
                }
            }
        } else {
            compile_error!("not implemented for platform")
        }
    }
}

fn decode_datetime(input: &Timestamp) -> Result<DateTime<Utc>, ParseError> {
    Ok(DateTime::from_utc(
        NaiveDateTime::from_timestamp_opt(input.seconds, input.nanos as u32).ok_or(ParseError::InvalidTimestamp)?,
        Utc,
    ))
}

fn encode_datetime(input: &DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: input.timestamp(),
        nanos: input.timestamp_subsec_nanos() as i32,
    }
}

#[derive(Error, Debug)]
pub enum ParseError {
    #[error("missing field {0:?}")]
    MissingField(&'static str),
    #[error("unknown value for enum {0:?}")]
    UnknownEnumValue(&'static str),
    #[error("invalid timestamp")]
    InvalidTimestamp,
    #[error("invalid label: {0}")]
    Label(#[from] label::ParseError),
    #[error("invalid encoded Windows NT filename")]
    InvalidNtFilename,
}
