use std::{collections::BTreeMap, fs::File, sync::Arc};

use anyhow::Result;

use cealn_action_context::Context;
use cealn_data::{
    depmap::ConcreteDepmapReference,
    file_entry::{FileEntry, FileEntryRef}, label::LabelPath,
};
use cealn_depset::DepMap;

pub(super) struct Vfs<C> {
    context: C,
    mounts: BTreeMap<String, ConcreteDepmapReference>,
}

pub(super) trait GenVfs: Send + Sync + 'static {
    fn open_file(&self, path: &str) -> Result<Option<File>>;
}

impl<C> Vfs<C>
where
    C: Context,
{
    pub fn new(context: C) -> Self {
        Vfs {
            context,
            mounts: Default::default(),
        }
    }

    pub fn mount(&mut self, mount: &str, depmap: ConcreteDepmapReference) {
        self.mounts.insert(mount.to_owned(), depmap);
    }
}

impl<C> GenVfs for Vfs<C>
where
    C: Context,
{
    fn open_file(&self, path: &str) -> Result<Option<File>> {
        // FIXME: handle non-normalized paths here?
        for (prefix, reference) in self.mounts.iter().rev() {
            let Some(subpath) = path.strip_prefix(prefix) else {
                continue;
            };
            if !(prefix.ends_with("/") || subpath.starts_with("/") || subpath.is_empty()) {
                continue;
            }
            let Ok(subpath) = LabelPath::new(subpath) else {
                continue;
            };
            let Some(subpath) = subpath.normalize_require_descending() else {
                continue;
            };

            let file_path = futures::executor::block_on(async {
                let depmap = self.context.lookup_concrete_depmap_force_directory(&reference).await?;

                let Some(entry) = depmap.get(subpath.as_ref())? else {
                    return Ok::<_, anyhow::Error>(None);
                };

                match entry {
                    FileEntryRef::Regular {
                        content_hash,
                        executable,
                    } => {
                        let cachefile = self.context.open_cache_file(content_hash, executable).await?;
                        Ok(Some(cachefile.to_owned()))
                    }
                    FileEntryRef::Symlink(_) => todo!(),
                    FileEntryRef::Directory => todo!(),
                }
            })?;

            let Some(file_path) = file_path else { return Ok(None) };

            let file = File::open(&file_path)?;

            return Ok(Some(file));
        }

        Ok(None)
    }
}
