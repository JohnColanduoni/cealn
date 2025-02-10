use serde::{Deserialize, Serialize};

use cealn_data::{package::Package, LabelBuf};

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct LoadPackageIn {
    pub package: LabelBuf,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub struct LoadPackageOut {
    pub package: Package,
}
