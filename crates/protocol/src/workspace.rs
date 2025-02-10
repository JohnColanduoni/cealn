use std::collections::BTreeMap;

use cealn_data::WorkspaceNameBuf;

/// A description of a loaded Workspace
#[derive(Clone, Debug)]
pub struct Workspace {
    me: ConcreteReference,

    /// Bindings of workspace names to concrete workspaces
    workspace_bindings: BTreeMap<WorkspaceNameBuf, ConcreteReference>,
}

/// A reference that uniquely defines a Workspace
#[derive(Clone, Debug)]
pub struct ConcreteReference {
    name: WorkspaceNameBuf,
    version: String,
}
