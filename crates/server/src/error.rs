use std::{fmt, sync::Arc};

use crate::graph::GenQueryNode;

pub(crate) struct QueryCallErrorContext {
    pub called_query: Arc<dyn GenQueryNode>,
    pub calling_query: Arc<dyn GenQueryNode>,
}

impl fmt::Display for QueryCallErrorContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "error in query call\n{:#?}\nvia\n{:#?}",
            self.called_query, self.calling_query
        )
    }
}
