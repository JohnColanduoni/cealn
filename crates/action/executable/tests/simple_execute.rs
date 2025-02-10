mod util;

use std::{collections::HashMap, path::PathBuf, str::FromStr};

use anyhow::{anyhow, bail};
use cealn_depset::depmap::DepMap;
use futures::task::SpawnExt;
use tempfile::TempDir;
use uuid::Uuid;

use cealn_data::{
    action::{ExecutePlatform, LinuxExecutePlatform, Run},
    depmap::{ConcreteFiletreeType, DepmapReference, LabelFiletreeType},
    LabelBuf,
};

pub fn main() {
    let ubuntu_sysroot = TempDir::new().unwrap();
    let ubuntu_sysroot_label_root = LabelBuf::new("//ubuntu_sysroot").unwrap();
    util::extract_docker_root(
        "ubuntu@sha256:b3e2e47d016c08b3396b5ebe06ab0b711c34e7f37b98c9d37abe794b71cea0a2",
        ubuntu_sysroot.path(),
    );

    let thread_pool = futures::executor::ThreadPool::builder().pool_size(4).create().unwrap();

    let input_tree_depmap_ref = todo!();
    let sysroot_depmap_ref = todo!();
    let sysroot_depmap = DepMap::builder().insert("/", &ubuntu_sysroot_label_root).build();

    let mut depmaps = HashMap::new();
    depmaps.insert(sysroot_depmap_ref, sysroot_depmap);

    let context = Context {
        thread_pool: thread_pool.clone(),
        build_fs_cache_dir: std::env::temp_dir(),
        depmaps,

        sysroot_path: ubuntu_sysroot.path().to_owned(),
        sysroot_label_root: ubuntu_sysroot_label_root.clone(),
    };

    let handle = thread_pool
        .spawn_with_handle(async move {
            let execute_program = Run::<LabelFiletreeType> {
                executable_path: "/bin/ls".to_owned(),

                args: vec![],

                input_tree: input_tree_depmap_ref,

                platform: ExecutePlatform::Linux(LinuxExecutePlatform {
                    execution_sysroot: sysroot_depmap_ref,

                    execution_sysroot_execroot_dest: "/tmp".to_owned(),
                }),
            };
            let (done, _handle) = cealn_action_executable::run(context, &execute_program);
            let status = done.await.unwrap();
            assert!(status.success(), "exited with {:?}", status);
        })
        .unwrap();
    futures::executor::block_on(handle);
}

pub struct Context {
    thread_pool: futures::executor::ThreadPool,
    build_fs_cache_dir: PathBuf,
    depmaps: HashMap<DepmapReference<LabelFiletreeType>, LabelFiletree>,

    sysroot_path: PathBuf,
    sysroot_label_root: LabelBuf,
}

impl cealn_action_executable::Context for Context {
    fn build_fs_cache_dir(&self) -> &std::path::Path {
        &self.build_fs_cache_dir
    }

    fn lookup_file_label(&self, label: &cealn_data::Label) -> anyhow::Result<std::path::PathBuf> {
        if label == &*self.sysroot_label_root {
            Ok(self.sysroot_path.clone())
        } else {
            bail!("unexpected label {:?}", label)
        }
    }

    fn get_file_depmap(&self, reference: &DepmapReference<LabelFiletreeType>) -> anyhow::Result<LabelFiletree> {
        self.depmaps
            .get(reference)
            .cloned()
            .ok_or_else(|| anyhow!("unexpected depmap reference"))
    }
}
