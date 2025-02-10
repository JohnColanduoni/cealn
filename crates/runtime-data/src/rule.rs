use std::collections::BTreeMap;

use cealn_data::{
    action::{ActionOutput, LabelAction},
    depmap::DepmapHash,
    file_entry::FileHash,
    label::LabelPathBuf,
    reference::Reference,
    rule::{Analysis, BuildConfig, Provider, Target},
    LabelBuf,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct PrepareRuleIn {
    pub rule: Reference,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PrepareRuleOut {}

#[derive(Serialize, Deserialize, Debug)]
pub struct StartAnalyzeTargetIn {
    pub target: Target,
    pub target_label: LabelBuf,
    pub build_config: BuildConfig,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct StartAnalyzeTargetOut {}

#[derive(Serialize, Deserialize, Debug)]
pub struct PollAnalyzeTargetIn {
    pub event: AnalyzeEvent,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
pub enum PollAnalyzeTargetOut {
    Done(Analysis),
    Requests(Vec<AnalyzeAsyncRequest>),
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum AnalyzeEvent {
    FirstPoll,
    LabeledFileIsSourceFile,
    Provider { provider: Provider },
    Providers { providers: Vec<Provider> },
    ActionOutput(ActionOutput),
    FileHandle { fileno: i32 },
    FilenameList { filenames: Vec<LabelPathBuf> },
    Boolean { value: bool },
    None,
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(rename_all = "snake_case")]
#[serde(tag = "type")]
pub enum AnalyzeAsyncRequest {
    LoadProviders {
        target: LabelBuf,
        build_config: BuildConfig,
    },
    LoadGlobalProvider {
        provider: Reference,
        build_config: BuildConfig,
    },
    ActionOutput {
        action: LabelAction,
        partial_actions: Vec<LabelAction>,
    },
    LabelOpen {
        label: LabelBuf,
    },
    ConcreteDepmapFileOpen {
        depmap: DepmapHash,
        filename: LabelPathBuf,
    },
    ConcreteDepmapDirectoryList {
        depmap: DepmapHash,
        filename: LabelPathBuf,
    },
    ContentRefOpen {
        hash: FileHash,
    },
    TargetExists {
        label: LabelBuf,
    },
    FileExists {
        label: LabelBuf,
    },
    IsFile {
        label: LabelBuf,
    },
}
