use std::{
    fmt,
    io::{self, SeekFrom},
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};

use async_trait::async_trait;

use cealn_data::Label;
use cealn_runtime::api::{types, Handle as ApiHandle, HandleRights, Result as WasiResult};
use cealn_runtime_virt::fs::system;
use cealn_source::SourceReference;

pub struct SourceFs {
    root: Arc<dyn ApiHandle>,
    _system_fs: system::SystemFs,
}

pub trait SourceReferenceHandler: Clone + Send + Sync + 'static {
    fn push(&self, item: SourceReference);
}

struct Shared<S>
where
    S: SourceReferenceHandler,
{
    references_tx: S,
    uncacheable_error: AtomicBool,
}

struct Handle<S>
where
    S: SourceReferenceHandler,
{
    inner: system::Handle,
    shared: Pin<Arc<Shared<S>>>,
    reference: SourceReference,
}

impl SourceFs {
    pub async fn new<S>(source_root: SourceReference, references_tx: S) -> anyhow::Result<Self>
    where
        S: SourceReferenceHandler,
    {
        let system_fs = system::SystemFs::new(source_root.full_file_path().to_owned()).await?;
        let shared = Arc::pin(Shared {
            references_tx,
            uncacheable_error: AtomicBool::new(false),
        });
        let root = Arc::new(Handle {
            inner: system_fs.root().into(),
            shared,
            reference: source_root,
        });

        Ok(SourceFs {
            _system_fs: system_fs,
            root,
        })
    }

    pub fn to_handle(&self) -> Arc<dyn ApiHandle> {
        self.root.clone()
    }
}

impl<S> Handle<S>
where
    S: SourceReferenceHandler,
{
    #[inline]
    fn register_uncacheable<T>(&self, result: WasiResult<T>) -> WasiResult<T> {
        match result {
            Ok(x) => Ok(x),
            Err(types::Errno::Io) => {
                // No ordering requirements, we only ever set this from false to true
                self.shared.uncacheable_error.store(true, Ordering::Relaxed);
                Err(types::Errno::Io)
            }
            Err(err) => Err(err),
        }
    }

    async fn reference_child(&self, path_segment: &str) -> WasiResult<SourceReference> {
        // FIXME: make async all the way up
        self.reference
            .reference_child(Label::new(path_segment).expect("should have been already limited to a single segment"))
            .await
            .map_err(|err| -> types::Errno {
                // FIXME: emit build event
                todo!("error referencing source: {}", err)
            })
    }

    fn emit_reference(&self, reference: SourceReference) {
        self.shared.references_tx.push(reference);
    }
}

#[async_trait]
impl<S> ApiHandle for Handle<S>
where
    S: SourceReferenceHandler,
{
    fn file_type(&self) -> types::Filetype {
        self.inner.file_type()
    }

    fn rights(&self) -> HandleRights {
        self.inner.rights()
    }

    async fn read(&self, iovs: &mut [io::IoSliceMut]) -> WasiResult<usize> {
        self.register_uncacheable(self.inner.read(iovs).await)
    }

    async fn write(&self, iovs: &[io::IoSlice]) -> WasiResult<usize> {
        self.register_uncacheable(self.inner.write(iovs).await)
    }

    async fn tell(&self) -> WasiResult<types::Filesize> {
        self.register_uncacheable(self.inner.tell().await)
    }

    async fn seek(&self, pos: SeekFrom) -> WasiResult<u64> {
        self.register_uncacheable(self.inner.seek(pos).await)
    }

    async fn openat_child(
        &self,
        path_segment: &str,
        read: bool,
        write: bool,
        oflags: types::Oflags,
        fd_flags: types::Fdflags,
    ) -> WasiResult<Arc<dyn ApiHandle>> {
        match self.register_uncacheable(
            self.inner
                .openat_child(path_segment, read, write, oflags, fd_flags)
                .await,
        ) {
            Ok(handle) => {
                let reference = self.reference_child(path_segment).await?;
                self.emit_reference(reference.clone());
                Ok(Arc::new(Handle {
                    inner: handle,
                    shared: self.shared.clone(),
                    reference,
                }))
            }
            Err(err) => Err(err),
        }
    }

    async fn readdir<'a>(
        &'a self,
        cookie: types::Dircookie,
    ) -> WasiResult<Box<dyn Iterator<Item = WasiResult<(types::Dirent, String)>> + 'a>> {
        // Access was recorded when the directory was opened
        self.register_uncacheable(self.inner.readdir(cookie).await)
    }

    async fn readlinkat_child(&self, path_segment: &str) -> WasiResult<String> {
        // FIXME: record access
        self.inner.readlinkat_child(path_segment).await
    }

    async fn filestat(&self) -> WasiResult<types::Filestat> {
        self.register_uncacheable(self.inner.filestat().await)
    }

    async fn filestat_child(&self, path_segment: &str) -> WasiResult<types::Filestat> {
        let reference = self.reference_child(path_segment).await?;
        self.emit_reference(reference);
        self.inner.filestat_child(path_segment).await
    }

    fn fdstat(&self) -> WasiResult<types::Fdflags> {
        self.register_uncacheable(self.inner.fdstat())
    }
}

impl<S> fmt::Debug for Handle<S>
where
    S: SourceReferenceHandler,
{
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

#[derive(Clone, Default)]
pub struct SourceReferenceCollector {
    dest: Arc<Mutex<Vec<SourceReference>>>,
}

impl SourceReferenceCollector {
    pub fn try_unwrap(self) -> Result<Vec<SourceReference>, SourceReferenceCollector> {
        match Arc::try_unwrap(self.dest) {
            Ok(x) => Ok(x.into_inner().unwrap()),
            Err(dest) => Err(SourceReferenceCollector { dest }),
        }
    }
}

impl SourceReferenceHandler for SourceReferenceCollector {
    fn push(&self, item: SourceReference) {
        self.dest.lock().unwrap().push(item);
    }
}
