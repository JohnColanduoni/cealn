use std::{
    convert::{TryFrom, TryInto},
    marker::PhantomData,
    ops::{Deref, DerefMut},
    slice,
    sync::Arc,
};

use anyhow::{bail, Context};
use serde::{de::DeserializeOwned, Serialize};

use wasmtime::{Extern, Instance as WasmInstance, Linker, Memory, Store, TypedFunc};

use cealn_runtime_data::{
    package_load::{LoadPackageIn, LoadPackageOut},
    rule::{
        PollAnalyzeTargetIn, PollAnalyzeTargetOut, PrepareRuleIn, PrepareRuleOut, StartAnalyzeTargetIn,
        StartAnalyzeTargetOut,
    },
    workspace_load::LoadRootWorkspaceOut,
    DataEncoding, InvocationResult,
};

use crate::{
    api::{types, wasi::WasiCtx, Api, Handle, InjectFdError},
    Interpreter,
};

pub struct Instance<A: Api> {
    store: Store<WasiCtx>,
    instance: WasmInstance,
    exports: Exports,
    _phantom: PhantomData<A>,
}

struct Exports {
    alloc_input_buffer: TypedFunc<u32, u32>,
    free_output_buffer: TypedFunc<u32, ()>,
}

pub struct Builder<A> {
    interpreter: Interpreter,
    api_context: WasiCtx,
    _phantom: PhantomData<A>,
}

const ALLOC_INPUT_BUFFER_ENTRY_POINT: &str = "cealn_alloc_input_buffer";
const FREE_OUTPUT_BUFFER_ENTRY_POINT: &str = "cealn_free_output_buffer";
const LOAD_ROOT_WORKSPACE_ENTRY_POINT: &str = "cealn_load_root_workspace";
const LOAD_PACKAGE_ENTRY_POINT: &str = "cealn_load_package";
const PREPARE_RULE_ENTRY_POINT: &str = "cealn_prepare_rule";
const START_ANALYZE_TARGET_ENTRY_POINT: &str = "cealn_start_analyze_target";
const POLL_ANALYZE_TARGET_ENTRY_POINT: &str = "cealn_poll_analyze_target";

impl<A: Api> Instance<A> {
    pub fn builder(interpreter: &Interpreter, api: A) -> anyhow::Result<Builder<A>> {
        Ok(Builder {
            interpreter: interpreter.clone(),
            api_context: WasiCtx::new(interpreter, api)?,
            _phantom: PhantomData,
        })
    }

    pub(crate) fn primary_memory<'a>(&'a mut self) -> MemoryGuard<'a> {
        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .expect("missing default memory");
        // We maintain an invaraint that the underlying instance can only be entered with a mutable reference to
        // our Instance object, and we don't expose memory objects otherwise, so this is safe.
        unsafe {
            MemoryGuard {
                bytes: slice::from_raw_parts(memory.data_ptr(&mut self.store), memory.data_size(&mut self.store)),
                _memory: memory,
            }
        }
    }

    pub(crate) fn primary_memory_mut<'a>(&'a mut self) -> MemoryGuardMut<'a> {
        let memory = self
            .instance
            .get_memory(&mut self.store, "memory")
            .expect("missing default memory");
        // We maintain an invaraint that the underlying instance can only be entered with a mutable reference to
        // our Instance object, and we don't expose memory objects otherwise, so this is safe.
        unsafe {
            MemoryGuardMut {
                bytes: slice::from_raw_parts_mut(memory.data_ptr(&mut self.store), memory.data_size(&mut self.store)),
                _memory: memory,
            }
        }
    }

    pub fn inject_fd(&mut self, handle: Arc<dyn Handle>) -> Result<types::Fd, InjectFdError> {
        self.store.data_mut().inject_fd(handle, None)
    }

    #[tracing::instrument(level = "debug", err, skip(self))]
    pub async fn load_root_workspace(&mut self) -> anyhow::Result<LoadRootWorkspaceOut> {
        let load_func = self
            .instance
            .get_func(&mut self.store, LOAD_ROOT_WORKSPACE_ENTRY_POINT)
            .context("missing load root workspace entry point")?;

        let load_func = load_func.typed::<(), u32>(&mut self.store)?;

        let pointer = load_func.call_async(&mut self.store, ()).await?;

        Ok(self
            .read_json_pointer::<InvocationResult<LoadRootWorkspaceOut>>(pointer)
            .await?
            .into_result()?)
    }

    #[tracing::instrument(level = "debug", err, skip(self))]
    pub async fn load_package(&mut self, input: &LoadPackageIn) -> anyhow::Result<LoadPackageOut> {
        let load_func = self
            .instance
            .get_func(&mut self.store, LOAD_PACKAGE_ENTRY_POINT)
            .context("missing load package entry point")?;

        let load_func = load_func.typed::<(u32,), u32>(&mut self.store)?;

        let input = self.write_json_pointer(input).await?;
        let output_pointer = load_func.call_async(&mut self.store, (input.pointer(),)).await?;

        Ok(self
            .read_json_pointer::<InvocationResult<LoadPackageOut>>(output_pointer)
            .await?
            .into_result()?)
    }

    #[tracing::instrument(level = "debug", err, skip(self, input))]
    pub async fn prepare_rule(&mut self, input: &PrepareRuleIn) -> anyhow::Result<PrepareRuleOut> {
        let prepare_func = self
            .instance
            .get_func(&mut self.store, PREPARE_RULE_ENTRY_POINT)
            .context("missing prepare rule entrypoint")?;

        let prepare_func = prepare_func.typed::<(u32,), u32>(&mut self.store)?;

        let input = self.write_json_pointer(input).await?;
        let output_pointer = prepare_func.call_async(&mut self.store, (input.pointer(),)).await?;

        Ok(self
            .read_json_pointer::<InvocationResult<PrepareRuleOut>>(output_pointer)
            .await?
            .into_result()?)
    }

    #[tracing::instrument(level = "debug", err, skip(self, input))]
    pub async fn start_analyze_target(
        &mut self,
        input: &StartAnalyzeTargetIn,
    ) -> anyhow::Result<StartAnalyzeTargetOut> {
        let start_func = self
            .instance
            .get_func(&mut self.store, START_ANALYZE_TARGET_ENTRY_POINT)
            .context("missing start analyze entrypoint")?;

        let start_func = start_func.typed::<(u32,), u32>(&mut self.store)?;

        let input = self.write_json_pointer(input).await?;
        let output_pointer = start_func.call_async(&mut self.store, (input.pointer(),)).await?;

        Ok(self
            .read_json_pointer::<InvocationResult<StartAnalyzeTargetOut>>(output_pointer)
            .await?
            .into_result()?)
    }

    #[tracing::instrument(level = "debug", err, skip(self, input))]
    pub async fn poll_analyze_target(&mut self, input: &PollAnalyzeTargetIn) -> anyhow::Result<PollAnalyzeTargetOut> {
        let start_func = self
            .instance
            .get_func(&mut self.store, POLL_ANALYZE_TARGET_ENTRY_POINT)
            .context("missing poll analyze entrypoint")?;

        let start_func = start_func.typed::<(u32,), u32>(&mut self.store)?;

        let input = self.write_json_pointer(input).await?;
        let result = start_func.call_async(&mut self.store, (input.pointer(),)).await;
        let output_pointer = result?;

        Ok(self
            .read_json_pointer::<InvocationResult<PollAnalyzeTargetOut>>(output_pointer)
            .await?
            .into_result()?)
    }

    async fn write_json_pointer<T: Serialize>(&mut self, value: T) -> anyhow::Result<InputBuffer> {
        let serialized_bytes = serde_json::to_vec(&value)?;
        let serialized_bytes_len = u32::try_from(serialized_bytes.len()).expect("serialized bytes too large");

        let slice_pointer = self
            .exports
            .alloc_input_buffer
            .call_async(&mut self.store, serialized_bytes_len)
            .await?;

        let bytes_pointer = u32::from_le_bytes(
            self.primary_memory()
                .get((slice_pointer as usize)..)
                .context("bad pointer")?
                .get(..4)
                .context("bad pointer")?
                .try_into()
                // We always fetch 4 bytes
                .unwrap(),
        );

        // Write bytes
        self.primary_memory_mut()
            .get_mut((bytes_pointer as usize)..)
            .context("bad pointer")?
            .get_mut(..serialized_bytes.len())
            .context("bad pointer")?
            .copy_from_slice(&serialized_bytes);

        // Set length
        self.primary_memory_mut()
            .get_mut((slice_pointer as usize)..)
            .context("bad pointer")?
            .get_mut(4..8)
            .context("bad pointer")?
            .copy_from_slice(&u32::to_le_bytes(serialized_bytes_len));

        Ok(InputBuffer { pointer: slice_pointer })
    }

    async fn read_json_pointer<T: DeserializeOwned>(&mut self, pointer: u32) -> anyhow::Result<T> {
        let bytes_pointer = u32::from_le_bytes(
            self.primary_memory()
                .get((pointer as usize)..(pointer as usize + 4))
                .context("bad pointer")?
                .try_into()
                // We always fetch 4 bytes
                .unwrap(),
        );
        let bytes_len = u32::from_le_bytes(
            self.primary_memory()
                .get((pointer as usize + 4)..(pointer as usize + 8))
                .context("bad pointer")?
                .try_into()
                // We always fetch 4 bytes
                .unwrap(),
        );
        let encoding = u32::from_le_bytes(
            self.primary_memory()
                .get((pointer as usize + 8)..(pointer as usize + 12))
                .context("bad pointer")?
                .try_into()
                // We always fetch 4 bytes
                .unwrap(),
        );
        let encoding = match encoding {
            1 => DataEncoding::Utf8,
            2 => DataEncoding::Latin1,
            _ => bail!("invalid data encoding"),
        };
        let bytes_end = bytes_pointer.checked_add(bytes_len).context("bad pointer")?;
        let memory = self.primary_memory();
        let bytes = memory
            .get((bytes_pointer as usize)..(bytes_end as usize))
            .context("bad pointer")?;

        let content = match encoding {
            DataEncoding::Utf8 => serde_json::from_slice::<T>(bytes)?,
            DataEncoding::Latin1 => {
                // Optimistically check if the bytes are all ascii, in which case we can use serde_json's native decode
                for b in bytes {
                    if *b > 0x7F {
                        todo!()
                    }
                }
                serde_json::from_slice::<T>(bytes)?
            }
        };

        self.exports
            .free_output_buffer
            .call_async(&mut self.store, pointer)
            .await?;

        Ok(content)
    }
}

struct InputBuffer {
    pointer: u32,
}

impl InputBuffer {
    fn pointer(&self) -> u32 {
        self.pointer
    }
}

impl<A: Api> Builder<A> {
    pub async fn build(self) -> anyhow::Result<Instance<A>> {
        let mut store = Store::new(self.interpreter.engine(), self.api_context);

        let mut linker = Linker::new(self.interpreter.engine());

        store.data().add_to_linker(&mut linker)?;

        let instance = linker.instantiate_async(&mut store, self.interpreter.module()).await?;

        let Some(Extern::Func(start_func)) = instance.get_export(&mut store, "_start") else {
            bail!("missing start symbol");
        };
        start_func.call_async(&mut store, &[], &mut []).await?;

        let exports = Exports::bind(&instance, &mut store)?;

        Ok(Instance {
            store,
            instance,
            exports,
            _phantom: PhantomData,
        })
    }

    /// Access the API context before the instance starts
    ///
    /// Allows modifying the API state (e.g. injecting standard file descriptors) before launch
    pub fn wasi_ctx(&self) -> &WasiCtx {
        &self.api_context
    }
}

impl Exports {
    fn bind(instance: &WasmInstance, store: &mut Store<WasiCtx>) -> anyhow::Result<Self> {
        let alloc_input_buffer = instance
            .get_func(&mut *store, ALLOC_INPUT_BUFFER_ENTRY_POINT)
            .context("missing allocate input buffer entrypoint")?;

        let alloc_input_buffer = alloc_input_buffer.typed::<u32, u32>(&mut *store)?;

        let free_output_buffer = instance
            .get_func(&mut *store, FREE_OUTPUT_BUFFER_ENTRY_POINT)
            .context("missing free input buffer entrypoint")?;

        let free_output_buffer = free_output_buffer.typed::<u32, ()>(&mut *store)?;

        Ok(Exports {
            alloc_input_buffer,
            free_output_buffer,
        })
    }
}

pub struct MemoryGuard<'a> {
    _memory: Memory,
    bytes: &'a [u8],
}

impl<'a> Deref for MemoryGuard<'a> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.bytes
    }
}

pub struct MemoryGuardMut<'a> {
    _memory: Memory,
    bytes: &'a mut [u8],
}

impl<'a> Deref for MemoryGuardMut<'a> {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.bytes
    }
}

impl<'a> DerefMut for MemoryGuardMut<'a> {
    fn deref_mut(&mut self) -> &mut [u8] {
        self.bytes
    }
}
