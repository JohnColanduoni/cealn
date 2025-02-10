use std::{mem, sync::Arc};

use anyhow::Result;
use cealn_event::EventContext;
use futures::{join, pin_mut, prelude::*};
use thiserror::Error;

use cealn_protocol::query::RootWorkspaceLoadQueryProduct;
use cealn_runtime::{
    api::{types, Api, ApiDispatch, Handle, InjectFdError},
    Instance, Interpreter,
};
use cealn_runtime_data::fs::WORKSPACE_MOUNT_PATH;

use tracing::{debug, info};

use crate::{
    executor::Executor,
    runtime::logger::{self, Logger},
};

/// An actively processing load operation for a workspace
pub struct Loader {
    runtime: Instance<LoaderApi>,
    logger: Option<Logger>,
}

impl Loader {
    #[tracing::instrument(level = "info", err, skip(python_interpreter, workspace_fs))]
    pub async fn new(
        python_interpreter: &Interpreter,
        events: EventContext,
        workspace_fs: Arc<dyn Handle>,
    ) -> Result<Self> {
        let api = LoaderApi(Arc::new(_LoaderApi {
            filesystems: vec![(WORKSPACE_MOUNT_PATH.to_owned(), workspace_fs)],
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
            logger: Some(logger),
        })
    }

    #[tracing::instrument(level = "info", err, skip(self, executor))]
    pub async fn load(mut self, executor: &Executor) -> Result<RootWorkspaceLoadQueryProduct> {
        let output = self.runtime.load_root_workspace().await?;

        let logger = self.logger.take().unwrap();
        // Drop runtime to allow stdio streams to finish
        mem::drop(self);
        let stdio = logger.finish();

        Ok(RootWorkspaceLoadQueryProduct {
            name: output.name,
            local_workspaces: output.local_workspaces,
            global_default_providers: output.global_default_providers,
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
    fn load_empty_workspace() {
        cealn_test_util::prep();

        let mut test_workspace_path = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        test_workspace_path.push("test-support/empty_package");

        let interpreter = python_interpreter_for_testing();
        let executor = executor_for_testing();
        let workspace_fs = SystemFs::new(test_workspace_path).unwrap();
        let loader = Loader::new(interpreter, workspace_fs.root()).unwrap();
        let _product = futures::executor::block_on(loader.load(&executor)).unwrap();
    }
}
