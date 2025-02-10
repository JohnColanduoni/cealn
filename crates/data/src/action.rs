mod docker;
mod exec;
mod extract;
mod git;
mod net;

use std::{
    any::TypeId,
    borrow::Cow,
    collections::{BTreeMap, HashMap, VecDeque},
    hash::Hash,
    marker::PhantomData,
    mem,
    str::FromStr,
    sync::Arc,
};

use indexmap::IndexMap;
use jsonpath_rust::JsonPathInst;
use serde::{
    de::{
        value::{MapAccessDeserializer, MapDeserializer},
        Error as DeError, Visitor,
    },
    ser::SerializeMap,
    Deserialize, Serialize,
};

use crate::{
    action::exec::MacOSExecutePlatform,
    cache::Cacheability,
    depmap::{ConcreteDepmapReference, ConcreteFiletreeType, DepmapHash, DepmapType, LabelFiletreeType},
    file_entry::FileHash,
    label::{LabelPathBuf, NormalizedDescending, LABEL_SENTINEL},
    reference::Reference,
    rule::BuildConfig,
    Label, LabelBuf,
};

pub use self::{
    docker::DockerDownload,
    exec::{Executable, ExecutePlatform, LinuxExecutePlatform},
    extract::Extract,
    git::GitClone,
    net::{Download, DownloadFileDigest},
};

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Action<I: DepmapType> {
    pub id: String,

    pub mnemonic: String,
    pub progress_message: String,

    #[serde(flatten)]
    pub data: ActionData<I>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(tag = "$cealn_action")]
#[serde(rename_all = "snake_case")]
#[serde(bound = "")]
pub enum ActionData<I: DepmapType> {
    Run(Run<I>),
    Download(Download),
    DockerDownload(DockerDownload),
    Extract(Extract<I>),
    GitClone(GitClone),
    BuildDepmap(BuildDepmap<I>),
    Transition(Transition),
}

pub type ConcreteAction = Action<ConcreteFiletreeType>;
pub type LabelAction = Action<LabelFiletreeType>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ActionOutput {
    pub files: DepmapHash,

    pub stdout: Option<FileHash>,
    pub stderr: Option<FileHash>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Run<I: DepmapType> {
    /// Path to executable within input tree
    pub executable: Executable<I>,

    pub args: Vec<ArgumentSource<I>>,
    pub cwd: Option<String>,
    pub append_env: BTreeMap<String, String>,
    pub append_env_files: Vec<I::DepmapReference>,

    pub input: Option<I::DepmapReference>,

    pub platform: ExecutePlatform<I>,

    pub hide_stdout: bool,
    pub hide_stderr: bool,
    pub structured_messages: Option<StructuredMessageConfig>,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct StructuredMessageConfig {
    pub level_map: IndexMap<JsonPath, StructuredMessageLevel>,
    pub human_messages: Vec<JsonPath>,
}

#[derive(Clone, Debug)]
pub struct JsonPath {
    original: String,
    parsed: jsonpath_rust::parser::model::JsonPath,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum StructuredMessageLevel {
    Error,
    Warn,
    Info,
    Debug,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum ArgumentSource<I: DepmapType> {
    Literal(String),
    Label(I::DepmapReference),
    Templated {
        template: String,
        source: I::DepmapReference,
    },
    Respfile {
        template: String,
        source: I::DepmapReference,
    },
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct BuildDepmap<I: DepmapType> {
    pub entries: Vec<(NormalizedDescending<LabelPathBuf>, BuildDepmapEntry<I>)>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Transition {
    pub label: LabelBuf,
    pub changed_options: Vec<(Reference, Reference)>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
#[serde(bound = "")]
#[serde(rename_all = "snake_case")]
pub enum BuildDepmapEntry<I: DepmapType> {
    Reference(I::DepmapReference),
    Directory,
    File {
        content: Arc<str>,
        executable: bool,
    },
    Symlink {
        target: String,
    },
    Filter {
        base: I::DepmapReference,
        prefix: LabelPathBuf,
        patterns: Vec<String>,
    },
}

impl<I: DepmapType> Action<I> {
    /// Obtains the cacheability induced by this actions own parameters
    pub fn inherent_cacheability(&self) -> Cacheability {
        match &self.data {
            ActionData::Run(_) => Cacheability::Global,
            ActionData::Download(v) => {
                if v.digest.is_some() {
                    Cacheability::Global
                } else {
                    Cacheability::Private
                }
            }
            ActionData::GitClone(_) => Cacheability::Global,
            // FIXME: Do this better
            ActionData::DockerDownload(v) => {
                if v.image.contains("@sha256:") {
                    Cacheability::Global
                } else {
                    Cacheability::Private
                }
            }
            ActionData::Extract(_) => Cacheability::Global,
            ActionData::BuildDepmap(_) => Cacheability::Global,
            ActionData::Transition(_) => Cacheability::Global,
        }
    }
}

impl Action<LabelFiletreeType> {
    pub fn source_depmaps<'a>(
        &'a self,
        build_config: &'a BuildConfig,
    ) -> Box<dyn Iterator<Item = (&'a Label, Option<BuildConfig>)> + Send + 'a> {
        match &self.data {
            ActionData::Run(run) => {
                let mut actions = Vec::new();
                actions.extend(run.input.as_deref().map(|label| (label, None)));
                let host_build_config = BuildConfig {
                    options: build_config.host_options.clone(),
                    host_options: build_config.host_options.clone(),
                };
                actions.extend(
                    run.executable
                        .context
                        .as_deref()
                        .map(|label| (label, Some(host_build_config.clone()))),
                );
                actions.extend(run.args.iter().flat_map(enumerate_argument_source));
                match &run.platform {
                    ExecutePlatform::Linux(platform) => {
                        actions.push((&platform.execution_sysroot, Some(host_build_config.clone())))
                    }
                    ExecutePlatform::MacOS(platform) => {
                        actions.push((&platform.execution_sysroot_extra, Some(host_build_config.clone())))
                    }
                }
                actions.extend(run.append_env_files.iter().map(|x| (&**x, None)));
                Box::new(actions.into_iter())
            }
            ActionData::BuildDepmap(build_depmap) => {
                Box::new(build_depmap.entries.iter().filter_map(|(_, x)| match x {
                    BuildDepmapEntry::Reference(reference) => Some((&**reference, None)),
                    BuildDepmapEntry::Directory => None,
                    BuildDepmapEntry::File { .. } => None,
                    BuildDepmapEntry::Symlink { .. } => None,
                    BuildDepmapEntry::Filter { base, .. } => Some((&**base, None)),
                }))
            }
            ActionData::DockerDownload(_) => Box::new(vec![].into_iter()),
            ActionData::Download(_) => Box::new(vec![].into_iter()),
            ActionData::GitClone(_) => Box::new(vec![].into_iter()),
            ActionData::Extract(extract) => Box::new(vec![(&*extract.archive, None)].into_iter()),
            ActionData::Transition(transition) => Box::new(vec![(&*transition.label, None)].into_iter()),
        }
    }
}

fn enumerate_argument_source<'a>(
    arg: &'a ArgumentSource<LabelFiletreeType>,
) -> impl Iterator<Item = (&'a Label, Option<BuildConfig>)> + Send + 'a {
    match arg {
        ArgumentSource::Literal(_) => vec![].into_iter(),
        ArgumentSource::Label(label) => vec![(&**label, None)].into_iter(),
        ArgumentSource::Templated { source, .. } => vec![(&**source, None)].into_iter(),
        ArgumentSource::Respfile { source, .. } => vec![(&**source, None)].into_iter(),
    }
}

impl Action<LabelFiletreeType> {
    pub fn make_concrete<'a>(
        &'a self,
        source_depmaps: &HashMap<LabelBuf, ConcreteDepmapReference>,
    ) -> Action<ConcreteFiletreeType> {
        let data = match &self.data {
            ActionData::Run(run) => {
                let context = run.executable.context.as_ref().map(|depmap| {
                    source_depmaps
                        .get(depmap)
                        .expect("failed to resolve all source depmaps")
                });
                let input = run.input.as_ref().map(|depmap| {
                    source_depmaps
                        .get(depmap)
                        .expect("failed to resolve all source depmaps")
                });
                let args: Vec<_> = run
                    .args
                    .iter()
                    .map(|arg| map_argument_source(arg, source_depmaps))
                    .collect();
                let append_env_files: Vec<_> = run
                    .append_env_files
                    .iter()
                    .map(|depmap| {
                        source_depmaps
                            .get(depmap)
                            .cloned()
                            .expect("failed to resolve all source depmaps")
                    })
                    .collect();

                let platform = match &run.platform {
                    ExecutePlatform::Linux(platform) => {
                        let execution_sysroot = source_depmaps
                            .get(&platform.execution_sysroot)
                            .expect("failed to resolve all source depmaps");

                        ExecutePlatform::Linux(LinuxExecutePlatform {
                            execution_sysroot: execution_sysroot.clone(),
                            execution_sysroot_input_dest: platform.execution_sysroot_input_dest.clone(),
                            execution_sysroot_output_dest: platform.execution_sysroot_output_dest.clone(),
                            execution_sysroot_exec_context_dest: platform.execution_sysroot_exec_context_dest.clone(),
                            uid: platform.uid.clone(),
                            gid: platform.gid.clone(),
                            standard_environment_variables: platform.standard_environment_variables.clone(),
                            use_fuse: platform.use_fuse,
                            use_interceptor: platform.use_interceptor,
                        })
                    }
                    ExecutePlatform::MacOS(platform) => {
                        let execution_sysroot_extra = source_depmaps
                            .get(&platform.execution_sysroot_extra)
                            .expect("failed to resolve all source depmaps");

                        ExecutePlatform::MacOS(MacOSExecutePlatform {
                            execution_sysroot_extra: execution_sysroot_extra.clone(),
                        })
                    }
                };

                ActionData::Run(Run {
                    executable: Executable {
                        name: run.executable.name.clone(),
                        executable_path: run.executable.executable_path.clone(),
                        context: context.cloned(),
                        search_paths: run.executable.search_paths.clone(),
                        library_search_paths: run.executable.library_search_paths.clone(),
                    },
                    args,
                    cwd: run.cwd.clone(),
                    append_env: run.append_env.clone(),
                    append_env_files,
                    input: input.cloned(),
                    platform,
                    hide_stdout: run.hide_stdout,
                    hide_stderr: run.hide_stderr,
                    structured_messages: run.structured_messages.clone(),
                })
            }
            ActionData::BuildDepmap(value) => {
                let mut entries = Vec::new();
                for (k, v) in &value.entries {
                    match v {
                        BuildDepmapEntry::Reference(reference) => {
                            let reference = source_depmaps
                                .get(reference)
                                .expect("failed to resolve all source depmaps");
                            entries.push((k.clone(), BuildDepmapEntry::Reference(reference.clone())));
                        }
                        BuildDepmapEntry::Directory => {
                            entries.push((k.clone(), BuildDepmapEntry::Directory));
                        }
                        BuildDepmapEntry::File { content, executable } => {
                            entries.push((
                                k.clone(),
                                BuildDepmapEntry::File {
                                    content: content.clone(),
                                    executable: *executable,
                                },
                            ));
                        }
                        BuildDepmapEntry::Symlink { target } => {
                            entries.push((k.clone(), BuildDepmapEntry::Symlink { target: target.clone() }));
                        }
                        BuildDepmapEntry::Filter { base, prefix, patterns } => {
                            let base = source_depmaps.get(base).expect("failed to resolve all source depmaps");
                            entries.push((
                                k.clone(),
                                BuildDepmapEntry::Filter {
                                    base: base.clone(),
                                    prefix: prefix.clone(),
                                    patterns: patterns.clone(),
                                },
                            ));
                        }
                    }
                }
                ActionData::BuildDepmap(BuildDepmap { entries })
            }
            ActionData::DockerDownload(value) => ActionData::DockerDownload(value.clone()),
            ActionData::Download(value) => ActionData::Download(value.clone()),
            ActionData::GitClone(value) => ActionData::GitClone(value.clone()),
            ActionData::Extract(extract) => {
                let concrete_depmap = source_depmaps
                    .get(&extract.archive)
                    .expect("failed to resolve all source depmaps");
                ActionData::Extract(Extract {
                    archive: concrete_depmap.clone(),
                    strip_prefix: extract.strip_prefix.clone(),
                })
            }
            ActionData::Transition(_) => panic!("cannot make a transition concrete"),
        };
        Action {
            id: self.id.clone(),
            mnemonic: self.mnemonic.clone(),
            progress_message: self.progress_message.clone(),
            data,
        }
    }
}

fn map_argument_source(
    arg: &ArgumentSource<LabelFiletreeType>,
    source_depmaps: &HashMap<LabelBuf, ConcreteDepmapReference>,
) -> ArgumentSource<ConcreteFiletreeType> {
    match arg {
        ArgumentSource::Literal(literal) => ArgumentSource::Literal(literal.clone()),
        ArgumentSource::Label(label) => ArgumentSource::Label(
            source_depmaps
                .get(label)
                .expect("failed to resolve all source depmaps")
                .clone(),
        ),
        ArgumentSource::Templated { template, source } => ArgumentSource::Templated {
            template: template.clone(),
            source: source_depmaps
                .get(source)
                .expect("failed to resolve all source depmaps")
                .clone(),
        },
        ArgumentSource::Respfile { template, source } => ArgumentSource::Respfile {
            template: template.clone(),
            source: source_depmaps
                .get(source)
                .expect("failed to resolve all source depmaps")
                .clone(),
        },
    }
}

impl JsonPath {
    pub fn extract_match<'a>(&'a self, value: &'a serde_json::Value) -> Option<Cow<'a, serde_json::Value>> {
        // TODO: cache instance
        let instance = jsonpath_rust::path::json_path_instance(&self.parsed, value);
        // TODO: can we avoid to_owned here?
        let matches = instance.find(jsonpath_rust::JsonPathValue::NewValue(value.to_owned()));
        let mut matches: VecDeque<_> = matches.into();
        match matches.pop_front() {
            Some(jsonpath_rust::JsonPathValue::Slice(match_value, _)) => Some(Cow::Borrowed(match_value)),
            Some(jsonpath_rust::JsonPathValue::NewValue(match_value)) => Some(Cow::Owned(match_value)),
            _ => None,
        }
    }
}

impl<I: DepmapType> Serialize for ArgumentSource<I> {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            ArgumentSource::Literal(str) => serializer.serialize_str(str),
            ArgumentSource::Label(label) => label.serialize(serializer),
            ArgumentSource::Templated { template, source } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry(ARGUMENT_SOURCE_TEMPLATED_SENTINEL, template)?;
                map.serialize_entry("source", source)?;
                map.end()
            }
            ArgumentSource::Respfile { template, source } => {
                let mut map = serializer.serialize_map(Some(2))?;
                map.serialize_entry(ARGUMENT_SOURCE_RESPFILE_SENTINEL, template)?;
                map.serialize_entry("source", source)?;
                map.end()
            }
        }
    }
}

impl<'de, I: DepmapType> Deserialize<'de> for ArgumentSource<I> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(ArgumentSourceDeVisitor { _phantom: PhantomData })
    }
}

struct ArgumentSourceDeVisitor<I: DepmapType> {
    _phantom: PhantomData<I>,
}

impl<'de, I: DepmapType> Visitor<'de> for ArgumentSourceDeVisitor<I> {
    type Value = ArgumentSource<I>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(formatter, "argument source")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(ArgumentSource::Literal(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(ArgumentSource::Literal(v))
    }

    fn visit_map<A>(self, map: A) -> Result<Self::Value, A::Error>
    where
        A: serde::de::MapAccess<'de>,
    {
        // TODO: don't do a copy here
        let data = serde_json::Value::deserialize(MapAccessDeserializer::new(map))?;
        let serde_json::Value::Object(mut map) = data else {
            todo!()
        };
        if let Some(template) = map.remove(ARGUMENT_SOURCE_TEMPLATED_SENTINEL) {
            let serde_json::Value::String(template) = template else {
                todo!()
            };
            let Some(source) = map.remove("source") else {
                todo!()
            };
            Ok(ArgumentSource::Templated {
                template,
                source: serde_json::from_value(source).map_err(|err| A::Error::custom(err))?,
            })
        } else if let Some(template) = map.remove(ARGUMENT_SOURCE_RESPFILE_SENTINEL) {
            let serde_json::Value::String(template) = template else {
                todo!()
            };
            let Some(source) = map.remove("source") else {
                todo!()
            };
            Ok(ArgumentSource::Respfile {
                template,
                source: serde_json::from_value(source).map_err(|err| A::Error::custom(err))?,
            })
        } else {
            let reference = I::reference_deserialize_visit_map(MapDeserializer::new(map.into_iter()))
                .map_err(|err| A::Error::custom(err))?;
            Ok(ArgumentSource::Label(reference))
        }
    }
}

pub(crate) const ARGUMENT_SOURCE_TEMPLATED_SENTINEL: &str = "$cealn_argument_source_templated";
pub(crate) const ARGUMENT_SOURCE_RESPFILE_SENTINEL: &str = "$cealn_argument_source_respfile";

impl Hash for StructuredMessageConfig {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.level_map.len().hash(state);
        for (k, v) in &self.level_map {
            k.hash(state);
            v.hash(state);
        }
        self.human_messages.hash(state);
    }
}

impl FromStr for StructuredMessageLevel {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s {
            "error" => StructuredMessageLevel::Error,
            "warn" => StructuredMessageLevel::Warn,
            "info" => StructuredMessageLevel::Info,
            "debug" => StructuredMessageLevel::Debug,
            _ => return Err(format!("expected error, warn, info, or debug")),
        })
    }
}

impl PartialEq for JsonPath {
    fn eq(&self, other: &Self) -> bool {
        self.original == other.original
    }
}

impl Eq for JsonPath {}

impl Hash for JsonPath {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.original.hash(state);
    }
}

impl Serialize for JsonPath {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.original.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for JsonPath {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let original = String::deserialize(deserializer)?;
        let parsed = jsonpath_rust::parser::parser::parse_json_path(&original)
            .map_err(|err| D::Error::custom(format!("invalid json path: {}", err)))?;
        Ok(JsonPath { original, parsed })
    }
}
