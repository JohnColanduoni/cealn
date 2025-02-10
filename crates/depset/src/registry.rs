use std::{any::TypeId, hash::Hash, mem, sync::Arc};

use cealn_data::{
    depmap::{ConcreteFiletreeType, DepmapHash, DepmapType, LabelFiletreeType},
    file_entry::FileEntry,
    LabelBuf,
};
use dashmap::{mapref::entry::Entry, DashMap};

use crate::{ConcreteFiletree, DepMap, LabelFiletree};

#[derive(Clone)]
pub struct Registry {
    shared: Arc<_Registry>,
}

struct _Registry {
    concrete_filetree: TypedRegistry<ConcreteFiletreeType>,
    label_filetree: TypedRegistry<LabelFiletreeType>,
}

struct TypedRegistry<I: DepmapType> {
    map: DashMap<DepmapHash, DepMap<I::Key, I::Value>>,
}

impl Registry {
    pub fn new() -> Registry {
        Registry {
            shared: Arc::new(_Registry {
                concrete_filetree: TypedRegistry::new(),
                label_filetree: TypedRegistry::new(),
            }),
        }
    }

    pub fn register_filetree(&self, depmap: ConcreteFiletree) -> DepmapHash {
        self.shared.concrete_filetree.register(depmap)
    }

    pub fn register_label_filetree(&self, depmap: LabelFiletree) -> DepmapHash {
        self.shared.label_filetree.register(depmap)
    }

    pub fn get_filetree(&self, reference: &DepmapHash) -> Option<ConcreteFiletree> {
        self.shared.concrete_filetree.get(reference)
    }

    pub fn get_label_filetree(&self, reference: &DepmapHash) -> Option<LabelFiletree> {
        self.shared.label_filetree.get(reference)
    }

    pub fn get_filetree_generic<I: DepmapType>(&self, reference: &DepmapHash) -> Option<DepMap<I::Key, I::Value>> {
        // FIXME: disgusting
        if TypeId::of::<I>() == TypeId::of::<ConcreteFiletreeType>() {
            return unsafe { mem::transmute(self.get_filetree(reference)) };
        } else {
            todo!()
        }
    }

    pub fn register_filetree_generic<I: DepmapType>(&self, depmap: DepMap<I::Key, I::Value>) -> DepmapHash {
        // FIXME: disgusting
        if TypeId::of::<I>() == TypeId::of::<ConcreteFiletreeType>() {
            return unsafe { self.register_filetree(mem::transmute(depmap)) };
        } else {
            todo!()
        }
    }
}

impl<I: DepmapType> TypedRegistry<I> {
    fn new() -> Self {
        TypedRegistry {
            map: Default::default(),
        }
    }

    fn register(&self, depmap: DepMap<I::Key, I::Value>) -> DepmapHash {
        let hash = depmap.hash().clone();
        match self.map.entry(hash.clone()) {
            Entry::Occupied(_) => {
                // Prefer existing depmap, as doing so makes it less likely we'll have multiple copies of equivalent
                // depmaps around.
            }
            Entry::Vacant(entry) => {
                entry.insert(depmap);
            }
        }
        hash
    }

    fn get(&self, reference: &DepmapHash) -> Option<DepMap<I::Key, I::Value>> {
        self.map.get(reference).map(|x| (*x).clone())
    }
}
