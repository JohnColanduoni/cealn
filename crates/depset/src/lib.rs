#![feature(inherent_associated_types)]
#![feature(split_array)]

pub mod depmap;
pub mod registry;

use cealn_data::{
    file_entry::FileEntry,
    label::{LabelPathBuf, NormalizedDescending},
    LabelBuf,
};

pub use self::{depmap::DepMap, registry::Registry};

pub type ConcreteFiletree = DepMap<NormalizedDescending<LabelPathBuf>, FileEntry>;

pub type LabelFiletree = DepMap<NormalizedDescending<LabelPathBuf>, LabelBuf>;
