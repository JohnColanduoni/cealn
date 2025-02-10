use serde::{Deserialize, Serialize};

use crate::LabelBuf;

// A reference to a Python value (e.g. Rule or Provider class)
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct Reference {
    /// The label indicating the source file containing the source module
    pub source_label: LabelBuf,
    /// The qualified name within the module of the Python value
    pub qualname: String,
}
