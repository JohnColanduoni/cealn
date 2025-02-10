use std::{
    collections::{BTreeMap, HashMap},
    fmt::Write as _,
    fs::{self, File},
    io::{self, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

use cealn_rules_compose_data::{Image, Manifest, PortForward, Volume};
use clap::{Parser, Subcommand};
use ring::digest::SHA256;
use serde::{Deserialize, Serialize};

#[derive(Parser)]
struct Opts {
    #[clap(subcommand)]
    sub_command: SubCommand,
}

#[derive(Subcommand)]
enum SubCommand {
    Manifest(ManifestOpts),
    ImageTag(ImageTagOpts),
    KustomizeEntrypoint(KustomizeEntrypointOpts),
    CueEntrypoint(CueEntrypointOpts),
}

#[derive(Parser, Debug)]
pub struct ManifestOpts {
    #[clap(long)]
    default_repo: Option<String>,

    #[clap(name = "MANIFEST_OUT", required = true)]
    manifest_out: PathBuf,

    #[clap(name = "IMAGES_METADATA", required = true)]
    images_metadata: String,

    #[clap(name = "PORT_FORWARDS", required = true)]
    port_forwards: String,

    #[clap(name = "VOLUMES", required = true)]
    volumes: String,
}

#[derive(Parser, Debug)]
pub struct ImageTagOpts {
    #[clap(name = "IMAGE_TAG", required = true)]
    image_tag: String,

    #[clap(name = "IMAGE_METADATA", required = true)]
    image_metadata: String,
}

#[derive(Parser, Debug)]
pub struct KustomizeEntrypointOpts {
    #[clap(long)]
    default_repo: Option<String>,

    #[clap(long = "label")]
    labels: Vec<String>,

    #[clap(long = "substitution")]
    substitutions: Vec<String>,

    #[clap(name = "BASE")]
    bases: Vec<String>,
}

#[derive(Parser, Debug)]
pub struct CueEntrypointOpts {
    #[clap(long)]
    default_repo: Option<String>,

    #[clap(long = "label")]
    labels: Vec<String>,
}

fn main() {
    let opts = Opts::parse();

    match opts.sub_command {
        SubCommand::Manifest(manifest_opts) => manifest(manifest_opts),
        SubCommand::ImageTag(image_tag_opts) => image_tag(image_tag_opts),
        SubCommand::KustomizeEntrypoint(kustomize_entrypoint_opts) => kustomize_entrypoint(kustomize_entrypoint_opts),
        SubCommand::CueEntrypoint(cue_entrypoint_opts) => cue_entrypoint(cue_entrypoint_opts),
    }
}

fn manifest(manifest_opts: ManifestOpts) {
    let images_metadata: HashMap<String, ImageMetadata> = serde_json::from_str(&manifest_opts.images_metadata).unwrap();
    let port_forwards: Vec<PortForward> = serde_json::from_str(&manifest_opts.port_forwards).unwrap();
    let volumes: Vec<Volume> = serde_json::from_str(&manifest_opts.volumes).unwrap();

    let mut manifest = Manifest {
        images: Vec::new(),
        manifests: Vec::new(),
        volumes,
        port_forwards,
    };

    for (image_name, images_metadata) in &images_metadata {
        let entry_path = Path::new("images").join(image_name);

        let tag = fs::read_to_string(entry_path.join("tag.txt")).unwrap();

        manifest.images.push(Image {
            name: image_name.to_owned(),
            full_name: if let Some(default_repo) = manifest_opts.default_repo.as_ref() {
                format!("{}/{}", default_repo, image_name)
            } else {
                image_name.to_owned()
            },
            tag,
            layers: images_metadata
                .layers
                .iter()
                .map(|layer| match layer {
                    ImageLayer::Blob {
                        filename,
                        digest,
                        diff_id,
                        media_type,
                    } => cealn_rules_compose_data::ImageLayer::Blob {
                        filename: filename.to_owned(),
                        digest: digest.to_owned(),
                        diff_id: diff_id.to_owned(),
                        media_type: media_type.to_owned(),
                    },
                    ImageLayer::Loose(loose_path) => cealn_rules_compose_data::ImageLayer::Loose(loose_path.to_owned()),
                })
                .collect(),
            run_config: images_metadata.run_config.clone(),
        });
    }
    manifest.images.sort_by_key(|image| image.name.clone());

    manifest.manifests = glob::glob("manifests/**/*.yaml")
        .unwrap()
        .map(|p| p.unwrap().to_str().unwrap().to_owned())
        .collect();
    manifest.manifests.sort();

    let mut output = BufWriter::new(File::create(&manifest_opts.manifest_out).unwrap());
    serde_json::to_writer(&mut output, &manifest).unwrap();
    output.flush().unwrap();
}

fn image_tag(image_tag_opts: ImageTagOpts) {
    let metadata: ImageMetadata = serde_json::from_str(&image_tag_opts.image_metadata).unwrap();

    let mut hasher = ring::digest::Context::new(&SHA256);

    let run_config_bytes = serde_json::to_vec(&metadata.run_config).unwrap();
    hasher.update(&(run_config_bytes.len() as u64).to_le_bytes());
    hasher.update(&run_config_bytes);

    for layer in &metadata.layers {
        match layer {
            ImageLayer::Blob { digest, .. } => {
                hasher.update(digest.as_bytes());
            }
            ImageLayer::Loose(path) => {
                let mut loose_hasher = ring::digest::Context::new(&SHA256);
                let path = Path::new(path);
                if path.exists() {
                    hash_path(&mut loose_hasher, path);
                }
                let loose_digest = loose_hasher.finish();
                hasher.update(loose_digest.as_ref());
            }
        }
    }
    let digest = hasher.finish();

    let mut output = BufWriter::new(File::create("tag.txt").unwrap());
    output.write(hex::encode(digest.as_ref()).as_bytes()).unwrap();
    output.flush().unwrap();
}

fn hash_path(hasher: &mut ring::digest::Context, path: &Path) {
    if path.is_dir() {
        let mut entries = fs::read_dir(path).unwrap().collect::<Result<Vec<_>, _>>().unwrap();
        entries.sort_by_key(|x| x.file_name());

        hasher.update(&[1]);
        hasher.update(&(entries.len() as u64).to_le_bytes());
        for entry in &entries {
            let file_name = entry.file_name();
            let file_name = file_name.to_str().unwrap();
            hasher.update(&(file_name.len() as u64).to_le_bytes());
            hasher.update(file_name.as_bytes());
            hash_path(hasher, &entry.path());
        }
    } else {
        hasher.update(&[2]);
        let metadata = path.metadata().unwrap();

        hasher.update(&metadata.len().to_le_bytes());
        let mut reader = File::open(path).unwrap();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read_len = reader.read(&mut buffer).unwrap();
            if read_len == 0 {
                break;
            }
            hasher.update(&buffer[..read_len]);
        }
    }
}

#[derive(Serialize, Deserialize)]
struct ImageMetadata {
    layers: Vec<ImageLayer>,
    run_config: Option<oci_spec::image::Config>,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
enum ImageLayer {
    Blob {
        filename: String,
        digest: String,
        diff_id: String,
        media_type: String,
    },
    Loose(String),
}

fn kustomize_entrypoint(kustomize_entrypoint_opts: KustomizeEntrypointOpts) {
    let mut entrypoint = KustomizeEntrypoint {
        kind: "Kustomization".to_owned(),
        resources: kustomize_entrypoint_opts.bases,
        images: Vec::new(),
        labels: Vec::new(),
    };

    let mut entries = match fs::read_dir("images") {
        Ok(dir) => dir.collect::<Result<Vec<_>, _>>().unwrap(),
        Err(ref err) if err.kind() == io::ErrorKind::NotFound => Default::default(),
        Err(err) => panic!("failed to open images directory: {}", err),
    };
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let image_path = entry.path();
        let image_name = fs::read_to_string(image_path.join("name.txt")).unwrap();
        let tag = fs::read_to_string(image_path.join("tag.txt")).unwrap();
        entrypoint.images.push(KustomizeImage {
            name: image_name.to_owned(),
            new_name: if let Some(default_repo) = kustomize_entrypoint_opts.default_repo.as_ref() {
                format!("{}/{}", default_repo, image_name)
            } else {
                image_name
            },
            new_tag: tag,
        });
    }

    if !kustomize_entrypoint_opts.labels.is_empty() {
        let mut labels = KustomizeLabels {
            pairs: Default::default(),
            include_selectors: false,
            include_templates: true,
        };
        for label in &kustomize_entrypoint_opts.labels {
            let (k, v) = label.split_once('=').unwrap();
            labels.pairs.insert(k.to_owned(), v.to_owned());
        }
        entrypoint.labels.push(labels);
    }

    let mut output = BufWriter::new(File::create("kustomization.yaml").unwrap());
    serde_yaml::to_writer(&mut output, &entrypoint).unwrap();
    output.flush().unwrap();
}

#[derive(Serialize, Deserialize, Debug)]
struct KustomizeEntrypoint {
    kind: String,
    resources: Vec<String>,
    images: Vec<KustomizeImage>,
    labels: Vec<KustomizeLabels>,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct KustomizeImage {
    name: String,
    new_name: String,
    new_tag: String,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct KustomizeLabels {
    pairs: BTreeMap<String, String>,
    include_selectors: bool,
    include_templates: bool,
}

fn cue_entrypoint(cue_entrypoint_opts: CueEntrypointOpts) {
    let mut entrypoint = CueEntrypoint {
        images: Default::default(),
    };

    let mut entries = match fs::read_dir("images") {
        Ok(dir) => dir.collect::<Result<Vec<_>, _>>().unwrap(),
        Err(ref err) if err.kind() == io::ErrorKind::NotFound => Default::default(),
        Err(err) => panic!("failed to open images directory: {}", err),
    };
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let image_path = entry.path();
        let image_name = fs::read_to_string(image_path.join("name.txt")).unwrap();
        let tag = fs::read_to_string(image_path.join("tag.txt")).unwrap();
        let mut new_name = if let Some(default_repo) = cue_entrypoint_opts.default_repo.as_ref() {
            format!("{}/{}", default_repo, image_name)
        } else {
            image_name.clone()
        };
        write!(&mut new_name, ":{}", tag).unwrap();
        entrypoint.images.insert(image_name.to_owned(), new_name);
    }

    let cealn_compose_pkg_path = PathBuf::from("cue.mod/pkg/cealn.io/compose/build");
    fs::create_dir_all(&cealn_compose_pkg_path).unwrap();

    let mut output = BufWriter::new(File::create(cealn_compose_pkg_path.join("images.cue")).unwrap());
    write!(&mut output, "package build\n").unwrap();
    serde_json::to_writer(&mut output, &entrypoint).unwrap();
    output.flush().unwrap();
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct CueEntrypoint {
    images: BTreeMap<String, String>,
}
