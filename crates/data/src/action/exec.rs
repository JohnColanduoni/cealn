use std::collections::BTreeMap;

use cealn_data_derive_provider_serde::ProviderSerde;
use serde::{Deserialize, Serialize};

use crate::{depmap::DepmapType, Label};

#[derive(Clone, PartialEq, Eq, Hash, Debug, ProviderSerde)]
pub struct Executable<I: DepmapType> {
    pub name: Option<String>,
    pub executable_path: String,
    pub context: Option<I::DepmapReference>,
    pub search_paths: Vec<String>,
    pub library_search_paths: Vec<String>,
}

/// Holds platform-specific execution parameters
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ExecutePlatform<I: DepmapType> {
    Linux(LinuxExecutePlatform<I>),
    MacOS(MacOSExecutePlatform<I>),
}

/// Linux-specific execution parameters
#[derive(Clone, PartialEq, Eq, Hash, Debug, ProviderSerde)]
pub struct LinuxExecutePlatform<I: DepmapType> {
    /// The static files that make up the root filesystem in which the program will be executed
    pub execution_sysroot: I::DepmapReference,

    pub execution_sysroot_input_dest: String,
    pub execution_sysroot_output_dest: String,
    pub execution_sysroot_exec_context_dest: String,

    pub uid: u32,
    pub gid: u32,

    pub standard_environment_variables: BTreeMap<String, String>,

    pub use_fuse: bool,
    pub use_interceptor: bool,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, ProviderSerde)]
pub struct MacOSExecutePlatform<I: DepmapType> {
    pub execution_sysroot_extra: I::DepmapReference,
}

impl<I: DepmapType> Serialize for ExecutePlatform<I> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ExecutePlatform::Linux(linux) => linux.serialize(serializer),
            ExecutePlatform::MacOS(macos) => macos.serialize(serializer),
        }
    }
}

impl<'de, I: DepmapType> Deserialize<'de> for ExecutePlatform<I> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // FIXME: detect
        let linux = LinuxExecutePlatform::<I>::deserialize(deserializer)?;
        Ok(ExecutePlatform::Linux(linux))
    }
}
