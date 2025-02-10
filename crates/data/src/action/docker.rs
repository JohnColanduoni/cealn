use serde::{Deserialize, Serialize};

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub struct DockerDownload {
    pub image: String,
    pub architecture: String,
}
