pub(crate) mod action;
mod graph;
mod node;
mod result;

pub(crate) use self::node::GenQueryNode;
pub use self::{
    graph::Graph,
    result::{QueryError, QueryResult},
};

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct BuildConfigId(u64);
