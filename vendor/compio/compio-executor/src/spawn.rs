use std::{
    cell::Cell,
    marker::PhantomData,
    mem,
    ptr::NonNull,
    task::{self, Context},
    thread_local,
    time::Instant,
};

use future::BoxFuture;
use futures::{future::RemoteHandle, prelude::*};

/// Spawns a [`Future`](futures::Future) on the current executor
pub fn spawn<T>(task: T)
where
    T: Future<Output = ()> + Send + 'static,
{
    unsafe {
        let executor = CURRENT_EXECUTOR
            .clone()
            .expect("there is no exeuctor set for the current thread");
        let executor = executor.as_ref();
        executor.spawn(task.boxed());
    }
}

/// Spawns a [`Future`](futures::Future) that may make blocking calls on the current executor
pub fn spawn_blocking<T>(task: T)
where
    T: Future<Output = ()> + Send + 'static,
{
    // FIXME: actually spawn these differently
    unsafe {
        let executor = CURRENT_EXECUTOR
            .clone()
            .expect("there is no exeuctor set for the current thread");
        let executor = executor.as_ref();
        executor.spawn(task.boxed());
    }
}

/// Spawns a [`Future`](futures::Future) on the current executor, returning a corresponding
/// [`RemoteHandle`](futures::future::RemoteHandle)
pub fn spawn_handle<T>(task: T) -> RemoteHandle<T::Output>
where
    T: Future + Send + 'static,
    T::Output: Send + 'static,
{
    unsafe {
        let executor = CURRENT_EXECUTOR
            .clone()
            .expect("there is no exeuctor set for the current thread");
        let executor = executor.as_ref();
        let (runner, handle) = task.remote_handle();
        executor.spawn(runner.boxed());
        handle
    }
}

/// Spawns a [`Future`](futures::Future) that may make blocking calls on the current executor
pub fn spawn_blocking_handle<T>(task: T) -> RemoteHandle<T::Output>
where
    T: Future + Send + 'static,
    T::Output: Send + 'static,
{
    // FIXME: actually spawn these differently
    unsafe {
        let executor = CURRENT_EXECUTOR
            .clone()
            .expect("there is no exeuctor set for the current thread");
        let executor = executor.as_ref();
        let (runner, handle) = task.remote_handle();
        executor.spawn(runner.boxed());
        handle
    }
}

pub fn block<T>(task: T) -> impl Future<Output = T::Output>
where
    T: Future,
{
    // FIXME: actually spawn these differently
    async move { task.await }
}

pub trait Executor: 'static {
    fn spawn(&self, future: BoxFuture<'static, ()>);
    fn wake_after(&self, deadline: Instant, cx: &mut task::Context);
}

/// Sets the current executor that will field requests from [`spawn`](self::spawn) for the duration of a closure
#[inline]
pub fn set_executor(executor: &dyn Executor) -> CurrentExecutorGuard {
    unsafe {
        let prev = mem::replace(
            &mut CURRENT_EXECUTOR,
            Some(NonNull::new_unchecked(
                executor as *const dyn Executor as *mut dyn Executor,
            )),
        );
        CurrentExecutorGuard {
            prev,
            _phantom: PhantomData,
        }
    }
}

pub struct CurrentExecutorGuard<'a> {
    prev: Option<NonNull<dyn Executor>>,
    _phantom: PhantomData<&'a dyn Executor>,
}

impl Drop for CurrentExecutorGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            mem::replace(&mut CURRENT_EXECUTOR, self.prev);
        }
    }
}

#[thread_local]
pub(crate) static mut CURRENT_EXECUTOR: Option<NonNull<dyn Executor>> = None;
