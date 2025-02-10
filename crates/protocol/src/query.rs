use std::{collections::BTreeMap, fmt::Debug, hash::Hash};

use cealn_data::{
    depmap::ConcreteDepmapReference,
    package::Package,
    reference::Reference,
    rule::{Analysis, BuildConfig},
    workspace::{GlobalDefaultProvider, LocalWorkspaceParams, LocalWorkspaceResolved},
    Label, LabelBuf,
};
use serde::Serialize;

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub enum Query {
    Output(OutputQuery),
    Analysis(AnalysisQuery),
    Load(LoadQuery),
}

pub trait QueryType: Clone + PartialEq + Eq + Hash + Debug + Send + Sync + 'static {
    type Product: Clone + Serialize + Debug + Send + Sync + 'static;

    const KIND: &'static str;

    #[inline]
    fn label(&self) -> Option<&Label> {
        None
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct OutputQuery {
    pub target_label: LabelBuf,
    pub build_config: BuildConfig,
}

#[derive(Clone, Serialize, Debug)]
pub struct OutputQueryProduct {
    pub reference: Option<ConcreteDepmapReference>,
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Debug)]
pub struct AnalysisQuery {
    pub target_label: LabelBuf,
    pub build_config: BuildConfig,
}

#[derive(Clone, Serialize, Debug)]
pub struct AnalysisQueryProduct {
    pub analysis: Analysis,
    pub output_mounts: BTreeMap<String, String>,
    pub stdio: Vec<StdioLine>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct LoadQuery {
    pub package: LabelBuf,
}

#[derive(Clone, Serialize, Debug)]
pub struct LoadQueryProduct {
    pub package: Package,
    pub stdio: Vec<StdioLine>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct RootWorkspaceLoadQuery;

#[derive(Clone, Serialize, Debug)]
pub struct RootWorkspaceLoadQueryProduct {
    pub name: String,
    pub local_workspaces: Vec<LocalWorkspaceParams>,
    pub global_default_providers: Vec<GlobalDefaultProvider>,
    pub stdio: Vec<StdioLine>,
}

#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct AllWorkspacesLoadQuery;

#[derive(Clone, Serialize, Debug)]
pub struct AllWorkspacesLoadQueryProduct {
    pub name: String,
    pub local_workspaces: Vec<LocalWorkspaceResolved>,
}

#[derive(Clone, Serialize, Debug)]
pub struct StdioLine {
    pub stream: StdioStreamType,
    pub contents: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Debug)]
pub enum StdioStreamType {
    Stdout,
    Stderr,
}

impl QueryType for OutputQuery {
    type Product = OutputQueryProduct;

    const KIND: &'static str = "output";

    #[inline]
    fn label(&self) -> Option<&Label> {
        Some(&self.target_label)
    }
}

impl QueryType for AnalysisQuery {
    type Product = AnalysisQueryProduct;

    const KIND: &'static str = "analysis";

    #[inline]
    fn label(&self) -> Option<&Label> {
        Some(&self.target_label)
    }
}

impl QueryType for LoadQuery {
    type Product = LoadQueryProduct;

    const KIND: &'static str = "load";

    #[inline]
    fn label(&self) -> Option<&Label> {
        Some(&self.package)
    }
}

impl QueryType for RootWorkspaceLoadQuery {
    type Product = RootWorkspaceLoadQueryProduct;

    const KIND: &'static str = "root_workspace";
}

impl QueryType for AllWorkspacesLoadQuery {
    type Product = AllWorkspacesLoadQueryProduct;

    const KIND: &'static str = "all_workspaces";
}
