use std::sync::Arc;

use async_trait::async_trait;
use cealn_runtime::api::{self, types, Handle, HandleRights};

pub fn new() -> Arc<dyn Handle> {
    Arc::new(NullHandle)
}

#[derive(Debug)]
struct NullHandle;

#[async_trait]
impl Handle for NullHandle {
    fn file_type(&self) -> types::Filetype {
        types::Filetype::CharacterDevice
    }

    fn rights(&self) -> cealn_runtime::api::HandleRights {
        HandleRights::from_base(types::Rights::FD_FILESTAT_GET | types::Rights::FD_READ | types::Rights::FD_WRITE)
    }

    async fn read(&self, _iovs: &mut [std::io::IoSliceMut]) -> api::Result<usize> {
        Ok(0)
    }

    async fn write(&self, _iovs: &[std::io::IoSlice]) -> api::Result<usize> {
        todo!()
    }
}
