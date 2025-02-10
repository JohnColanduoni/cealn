use std::{mem, sync::Arc};

use anyhow::Result;
use cealn_event::{BuildEventData, EventContext};
use futures::{join, pin_mut, prelude::*};
use thiserror::Error;

use cealn_data::LabelBuf;
use cealn_protocol::query::LoadQueryProduct;
use cealn_runtime::{
    api::{types, Api, ApiDispatch, Handle, InjectFdError},
    Instance, Interpreter,
};
use cealn_runtime_data::{fs::NAMED_WORKSPACE_MOUNT_PATH, package_load::LoadPackageIn};

use tracing::{debug, info};

use crate::{
    executor::Executor,
    runtime::logger::{self, Logger},
};

/// An actively processing load operation for a workspace
pub struct Loader {
    runtime: Instance<LoaderApi>,
    package: LabelBuf,
    logger: Option<Logger>,
}

impl Loader {
    #[tracing::instrument(level = "info", err, skip(python_interpreter, named_workspaces_fs))]
    pub async fn new(
        python_interpreter: &Interpreter,
        events: EventContext,
        named_workspaces_fs: Arc<dyn Handle>,
        package: LabelBuf,
    ) -> Result<Self> {
        let api = LoaderApi(Arc::new(_LoaderApi {
            filesystems: vec![(NAMED_WORKSPACE_MOUNT_PATH.to_owned(), named_workspaces_fs)],
        }));

        let builder = Instance::builder(python_interpreter, api)?;

        let (logger, stdout_handle, stderr_handle) = Logger::new(events);
        builder
            .wasi_ctx()
            .inject_fd(cealn_runtime_virt::fs::null::new(), Some(types::Fd::from(0)))?;
        builder.wasi_ctx().inject_fd(stdout_handle, Some(types::Fd::from(1)))?;
        builder.wasi_ctx().inject_fd(stderr_handle, Some(types::Fd::from(2)))?;

        let runtime = builder.build().await?;

        Ok(Loader {
            runtime,
            package,
            logger: Some(logger),
        })
    }

    #[tracing::instrument(level = "info", err, skip(self, executor), fields(package=?self.package))]
    pub async fn load(mut self, executor: &Executor, events: EventContext) -> Result<LoadQueryProduct> {
        let output = self
            .runtime
            .load_package(&LoadPackageIn {
                package: self.package.clone(),
            })
            .await?;

        let logger = self.logger.take().unwrap();
        // Drop runtime to allow stdio streams to finish
        mem::drop(self);
        let stdio = logger.finish();

        Ok(LoadQueryProduct {
            package: output.package,
            stdio,
        })
    }
}

#[derive(Clone)]
struct LoaderApi(Arc<_LoaderApi>);

struct _LoaderApi {
    filesystems: Vec<(String, Arc<dyn Handle>)>,
}

impl Api for LoaderApi {}

impl ApiDispatch for LoaderApi {
    fn filesystems(&self) -> &[(String, Arc<dyn Handle>)] {
        &self.0.filesystems
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use cealn_runtime_virt::fs::system::SystemFs;

    use crate::test_util::{executor_for_testing, python_interpreter_for_testing};

    use super::*;

    #[test]
    fn load_empty_package() {
        cealn_test_util::prep();

        let mut test_workspace_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        test_workspace_path.push("test-support/empty_package");
        let package_label = LabelBuf::new("//my_package").unwrap();

        let interpreter = python_interpreter_for_testing();
        let executor = executor_for_testing();
        let workspace_fs = SystemFs::new(test_workspace_path).unwrap();
        let loader = Loader::new(interpreter, workspace_fs.root(), package_label.clone()).unwrap();
        let (events, _) = EventContext::new();
        let product = futures::executor::block_on(loader.load(&executor, events)).unwrap();

        assert_eq!(product.package.label, package_label);
    }

    #[test]
    fn load_copy_rule_package() {
        cealn_test_util::prep();

        let mut test_workspace_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        test_workspace_path.push("test-support/copy_rule");
        let package_label = LabelBuf::new("//my_package").unwrap();

        let interpreter = python_interpreter_for_testing();
        let executor = executor_for_testing();
        let workspace_fs = SystemFs::new(test_workspace_path).unwrap();
        let loader = Loader::new(interpreter, workspace_fs.root(), package_label.clone()).unwrap();
        let (events, _) = EventContext::new();
        let product = futures::executor::block_on(loader.load(&executor, events)).unwrap();

        assert_eq!(product.package.label, package_label);
    }
}
