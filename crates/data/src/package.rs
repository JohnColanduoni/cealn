use serde::{Deserialize, Serialize};

use crate::{rule::Target, LabelBuf};

/// A set of [`Target`]s that make up a package
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Package {
    pub label: LabelBuf,
    pub targets: Vec<Target>,
}
