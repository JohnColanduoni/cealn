use std::convert::TryInto;

use serde::{Deserialize, Serialize};

use cealn_data::action::{ActionOutput, ConcreteAction};

use crate::hashing::hash_serializable;

#[derive(Serialize, Deserialize, Debug)]
pub(crate) struct ActionCacheEntry {
    pub action: ConcreteAction,
    pub output: ActionOutput,
}

#[derive(Debug)]
pub(crate) enum ActionDigest {
    Sha256([u8; 32]),
}

pub(crate) fn hash_action(action: &ConcreteAction) -> ActionDigest {
    // Skip metadata that doesn't affect action outputs
    let digest = hash_serializable(&action.data);
    ActionDigest::Sha256(digest.as_ref().try_into().unwrap())
}
