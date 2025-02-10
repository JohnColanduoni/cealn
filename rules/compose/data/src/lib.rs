use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub images: Vec<Image>,
    pub manifests: Vec<String>,
    pub volumes: Vec<Volume>,
    pub port_forwards: Vec<PortForward>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Image {
    pub name: String,
    pub full_name: String,
    pub tag: String,
    pub layers: Vec<ImageLayer>,
    pub run_config: Option<oci_spec::image::Config>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum ImageLayer {
    Blob {
        filename: String,
        digest: String,
        diff_id: String,
        media_type: String,
    },
    Loose(String),
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct PortForward {
    #[serde(rename = "type")]
    pub types: TypeMeta,
    pub resource: ResourceName,
    pub resource_port: u16,
    pub local_port: u16,
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct TypeMeta {
    pub api_version: String,
    pub kind: String,
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub struct ResourceName {
    pub name: String,
    pub namespace: Option<String>,
}

#[derive(Clone, Serialize, Deserialize, Debug)]
pub struct Volume {
    pub persistent_volume_claim: String,
    pub namespace: String,
    pub sync_pod: String,
    pub sync_pod_module: String,
}
