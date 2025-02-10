use std::{
    marker::PhantomPinned,
    pin::Pin,
    task::{Poll, Waker},
    time::Instant,
};

use futures::prelude::*;
use pin_project::pin_project;

use crate::spawn::{self, Executor, CURRENT_EXECUTOR};

#[inline]
pub fn sleep_until(deadline: Instant) -> Sleep {
    Sleep {
        deadline,
        stored_waker: None,
        _pinned: PhantomPinned,
    }
}

// TODO: this is kind of garbage, no actual cancellation or reset. Implement something more like Tokio's intrusive
// linked list.
#[pin_project]
pub struct Sleep {
    deadline: Instant,
    stored_waker: Option<Waker>,
    // Just here because we want to use a method that will require pinning later
    _pinned: PhantomPinned,
}

impl Sleep {
    #[inline]
    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    #[inline]
    pub fn is_elapsed(&self) -> bool {
        Instant::now() >= self.deadline
    }

    pub fn reset(self: Pin<&mut Self>, deadline: Instant) {
        let this = self.project();
        *this.deadline = deadline;
        *this.stored_waker = None;
    }
}

impl Future for Sleep {
    type Output = ();

    fn poll(self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        if self.is_elapsed() {
            return Poll::Ready(());
        }

        let this = self.project();

        if let Some(waker) = this.stored_waker {
            if waker.will_wake(cx.waker()) {
                return Poll::Pending;
            }
        }

        *this.stored_waker = Some(cx.waker().clone());
        unsafe {
            let executor = CURRENT_EXECUTOR
                .clone()
                .expect("there is no exeuctor set for the current thread");
            let executor = executor.as_ref();
            executor.wake_after(this.deadline.clone(), cx);
        }

        Poll::Pending
    }
}
