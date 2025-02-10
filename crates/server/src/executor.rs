use std::{
    io,
    mem::{self, ManuallyDrop},
    pin::Pin,
    sync::{Arc, Mutex},
    task::{Context, Poll, Waker},
};

use futures::{future::RemoteHandle, prelude::*, task::SpawnExt};
use slab::Slab;
use thiserror::Error;
use tracing::{debug_span, trace_span, Span};
use tracing_futures::Instrument;

/// Manages concurrency across all aspects of a build
///
/// The `Executor` apportions CPU resources to various actions and tasks executed during a build, both in spawned
/// processes and in the main process. It avoids running more concurrent CPU-bound tasks than the desired build
/// concurrency, while allowing higher concurrency for IO bound tasks. Callers are relied upon to specify what kind of
/// task they are spawning.
#[derive(Clone)]
pub struct Executor(Arc<_Executor>);

struct _Executor {
    thread_pool: compio_executor::ThreadPool,
    event_loop: ManuallyDrop<tokio::runtime::Runtime>,

    process_ticket_state: Mutex<ProcessTicketState>,
}

impl Drop for _Executor {
    fn drop(&mut self) {
        unsafe {
            let event_loop = ManuallyDrop::take(&mut self.event_loop);
            event_loop.shutdown_background();
        }
    }
}

struct ProcessTicketState {
    available_process_tickets: usize,
    waiters: Slab<Waker>,
}

pub struct ProcessTicket {
    shared: Arc<_Executor>,
}

impl Drop for ProcessTicket {
    fn drop(&mut self) {
        let wakers = {
            let mut process_ticket_state = self.shared.process_ticket_state.lock().unwrap();
            process_ticket_state.available_process_tickets += 1;
            mem::replace(&mut process_ticket_state.waiters, Slab::new())
        };
        for (_, waker) in wakers {
            waker.wake();
        }
    }
}

#[derive(Clone, Default, Debug)]
pub struct Options {
    pub thread_pool_concurrency: Option<usize>,
    pub process_concurrency: Option<usize>,
}

impl Executor {
    pub fn new(options: Options) -> anyhow::Result<Executor> {
        let mut builder = compio_executor::ThreadPool::builder();

        if let Some(concurrency) = options.thread_pool_concurrency {
            builder.worker_count(concurrency);
        }
        let process_concurrency = options.process_concurrency.unwrap_or_else(|| num_cpus::get());

        let event_loop = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();
        let event_loop_handle = event_loop.handle().clone();

        builder.thread_name(|index| format!("cealn-{:02}", index));
        builder.wrap_start({
            move |_, f| {
                let guard = event_loop_handle.enter();
                f()
            }
        });

        let thread_pool = builder.build()?;

        Ok(Executor(Arc::new(_Executor {
            thread_pool,
            event_loop: ManuallyDrop::new(event_loop),
            process_ticket_state: Mutex::new(ProcessTicketState {
                available_process_tickets: process_concurrency,
                waiters: Slab::new(),
            }),
        })))
    }

    /// Spawns a child task on the executor that runs a future on the thread pool.
    ///
    /// `ty` specifies a [`TaskType`] to inform the `Executor` of the expected behavior of the future, so it can
    /// schedule it appropriately.
    pub fn spawn_immediate<F>(&self, f: F) -> RemoteHandle<<F as Future>::Output>
    where
        F: Future + Send + 'static,
        <F as Future>::Output: Send,
    {
        let current_span = Span::current();

        self.0.thread_pool.spawn_with_handle(async move {
            let task_span = trace_span!(parent: current_span.clone(), "spawn_immediate");
            f.instrument(if task_span.is_disabled() {
                // If we have tracing disabled, ensure parent span relationship is still linked to new task
                current_span
            } else {
                task_span
            })
            .await
        })
    }

    /// Spawns an independent task on the executor that runs a future on the thread pool.
    ///
    /// `ty` specifies a [`TaskType`] to inform the `Executor` of the expected behavior of the future, so it can
    /// schedule it appropriately.
    pub fn spawn<F>(&self, f: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let current_span = Span::current();

        self.0.thread_pool.spawn(async move {
            let task_span = trace_span!("spawn");
            task_span.follows_from(current_span);
            if let Err(_) = std::panic::AssertUnwindSafe(f.instrument(task_span))
                .catch_unwind()
                .await
            {
                eprintln!("terminating on uncaught panic in task");
                // Ensure we exit on a panic in an independent task
                std::process::exit(101);
            }
        })
    }

    pub async fn acquire_process_ticket(&self) -> anyhow::Result<ProcessTicket> {
        AcquireProcessTicket {
            executor: self.0.clone(),
        }
        .await
    }
}

struct AcquireProcessTicket {
    executor: Arc<_Executor>,
}

impl Future for AcquireProcessTicket {
    type Output = anyhow::Result<ProcessTicket>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let mut process_ticket_state = self.executor.process_ticket_state.lock().unwrap();

        if process_ticket_state.available_process_tickets == 0 {
            process_ticket_state.waiters.insert(cx.waker().clone());
            Poll::Pending
        } else {
            process_ticket_state.available_process_tickets -= 1;
            Poll::Ready(Ok(ProcessTicket {
                shared: self.executor.clone(),
            }))
        }
    }
}
