use std::{fs::File, path::PathBuf};

use cealn_action_context::{reqwest, Context};
use cealn_cache::HotDiskCache;
use cealn_data::{
    depmap::{ConcreteFiletreeDepmapReference, DepmapReference, LabelFiletreeDepmapReference},
    file_entry::{FileEntry, FileHash},
    LabelBuf,
};
use cealn_depset::depmap::DepMap;
use cealn_fs::Cachefile;

pub struct TestContext {
    http_client: reqwest::Client,
    _build_dir: tempfile::TempDir,
    tmp_dir: PathBuf,
    hot_cache: HotDiskCache,
    depmap_registry: cealn_depset::Registry,
}

impl TestContext {
    pub fn new() -> TestContext {
        let http_client = reqwest::Client::builder().pool_max_idle_per_host(0).build().unwrap();

        let build_dir = tempfile::TempDir::new().unwrap();
        let tmp_dir = build_dir.path().join("tmp");
        std::fs::create_dir_all(&tmp_dir).unwrap();

        let hot_cache = HotDiskCache::open(&build_dir.path().join("cache")).unwrap();

        TestContext {
            http_client,
            _build_dir: build_dir,
            tmp_dir,
            hot_cache,
            depmap_registry: cealn_depset::Registry::new(),
        }
    }
}

impl Context for TestContext {
    fn http_client(&self) -> &reqwest::Client {
        &self.http_client
    }

    fn tempfile(&self, description: &str) -> anyhow::Result<Cachefile> {
        todo!()
    }

    fn move_to_cache(&self, file: Cachefile) -> anyhow::Result<(FileHash, bool)> {
        self.hot_cache.move_to_cache(file)
    }

    fn move_to_cache_prehashed(&self, file: Cachefile, digest: &FileHash, exec: bool) -> anyhow::Result<()> {
        self.hot_cache.move_to_cache_prehashed(file, digest, exec)
    }

    fn register_concrete_filetree_depmap(
        &self,
        depmap: ConcreteFiletree,
    ) -> anyhow::Result<ConcreteFiletreeDepmapReference> {
        Ok(self.depmap_registry.register_filetree(depmap))
    }

    fn register_label_filetree_depmap(&self, depmap: LabelFiletree) -> anyhow::Result<LabelFiletreeDepmapReference> {
        Ok(self.depmap_registry.register_label_filetree(depmap))
    }
}
