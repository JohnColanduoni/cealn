use cealn_data::workspace::{GlobalDefaultProvider, LocalWorkspaceParams};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct LoadRootWorkspaceOut {
    pub name: String,
    pub local_workspaces: Vec<LocalWorkspaceParams>,
    pub global_default_providers: Vec<GlobalDefaultProvider>,
}
