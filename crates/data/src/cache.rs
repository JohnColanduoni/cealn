use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Debug)]
pub enum Cacheability {
    /// Eligible to be inserted into caches shared between machines
    Global,
    /// Eligible to be inserted into machine-local caches
    Private,
    Uncacheable,
}
