use std::{mem, ops::Deref, sync::Arc};

/// A shared pointer which doesn't allocate until the first `clone`
pub struct LazyArc<T> {
    state: State<T>,
}

enum State<T> {
    Unique(T),
    Shared(Arc<T>),
    Swapping,
}

impl<T> LazyArc<T> {
    #[inline]
    pub fn new(value: T) -> Self {
        LazyArc {
            state: State::Unique(value),
        }
    }

    #[inline]
    pub fn clone(&mut self) -> Self {
        match &self.state {
            State::Unique(_) => match mem::replace(&mut self.state, State::Swapping) {
                State::Unique(value) => {
                    let arc = Arc::new(value);
                    self.state = State::Shared(arc.clone());
                    LazyArc {
                        state: State::Shared(arc),
                    }
                }
                _ => unreachable!(),
            },
            State::Shared(arc) => LazyArc {
                state: State::Shared(arc.clone()),
            },
            State::Swapping => unreachable!(),
        }
    }
}

impl<T> Deref for LazyArc<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        match &self.state {
            State::Unique(value) => value,
            State::Shared(arc) => &*arc,
            State::Swapping => unreachable!(),
        }
    }
}
