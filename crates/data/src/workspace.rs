use serde::{Deserialize, Serialize};

use crate::{reference::Reference, LabelBuf};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LocalWorkspaceParams {
    pub path: LabelBuf,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct LocalWorkspaceResolved {
    pub name: String,
    pub path: LabelBuf,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum GlobalDefaultProvider {
    Static {
        provider_type: Reference,
        providing_target: LabelBuf,
    },
}
