#![feature(split_array)]

pub mod action;
pub mod cache;
pub mod depmap;
pub mod file_entry;
pub mod label;
pub mod package;
pub mod reference;
pub mod rule;
pub mod workspace;

pub use crate::label::{Label, LabelBuf, WorkspaceName, WorkspaceNameBuf};
