use std::{path::PathBuf, sync::Arc};

use cealn_data::file_entry::FileEntry;
use cealn_depset::{ConcreteFiletree, DepMap};

use crate::MaterializeContext;

pub struct MaterializeCache {}

pub struct Materialized {}

impl MaterializeCache {
    pub fn new(cache_dir: PathBuf, context: Arc<dyn MaterializeContext>) -> MaterializeCache {
        MaterializeCache {}
    }
}

pub async fn materialize_for_output<C>(
    context: &C,
    output_path: &PathBuf,
    depmap: ConcreteFiletree,
) -> anyhow::Result<()>
where
    C: MaterializeContext,
{
    todo!()
}
