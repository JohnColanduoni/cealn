use serde::{Deserialize, Serialize};

use crate::{depmap::DepmapType, label::LabelPathBuf};

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Extract<I: DepmapType> {
    pub archive: I::DepmapReference,

    pub strip_prefix: Option<LabelPathBuf>,
}
