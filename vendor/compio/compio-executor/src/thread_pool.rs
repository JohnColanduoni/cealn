mod builder;
mod sleep;

use crate::thread_pool::sleep::{Sleep, WakeInstrunctions};

pub use self::builder::Builder;

use std::{
    borrow::BorrowMut,
    cell::{RefCell, UnsafeCell},
    io,
    mem::ManuallyDrop,
    ops::DerefMut,
    panic,
    pin::Pin,
    ptr::{self, NonNull},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    task::{Context, Poll, Wake, Waker},
    thread::JoinHandle,
    time::Duration,
};

use compio_core::{
    event_queue::{self, CustomEvent, EventQueueFactory},
    EventQueue,
};
use crossbeam_utils::CachePadded;
use futures::{future::RemoteHandle, prelude::*};
use rand::{prelude::Distribution, rngs::SmallRng, seq::SliceRandom, SeedableRng};

pub struct ThreadPool {
    shared: Arc<Shared>,
    worker_join_handles: Vec<JoinHandle<()>>,
}

pub struct Handle {
    shared: Arc<Shared>,
}

struct Shared {
    injector: crossbeam_deque::Injector<WorkItem>,
    workers: Vec<WorkerHandle>,
    shared_event_queue: Option<EventQueue>,
    sleep: CachePadded<Sleep>,
}

struct Worker {
    shared: Arc<Shared>,
    worker_shared: Arc<WorkerShared>,
    future_queue: crossbeam_deque::Worker<WorkItem>,
    worker_event_queue: Option<EventQueue>,
}

struct WorkerShared {
    sleeping: CachePadded<AtomicBool>,
}

struct WorkerHandle {
    shared: Arc<WorkerShared>,
    future_queue: crossbeam_deque::Stealer<WorkItem>,
    worker_event_queue: Option<event_queue::Handle>,
}

struct WorkItem {
    task: Arc<dyn GenTask>,
}

trait GenTask: Send + Sync + 'static {
    unsafe fn run(self: Arc<Self>);

    fn pool(&self) -> Arc<Shared>;
}

struct Task<F> {
    pool: Arc<Shared>,

    future: CachePadded<UnsafeCell<TaskFutureState<F>>>,
    wake_count: CachePadded<AtomicUsize>,
}

impl<F> Drop for Task<F> {
    fn drop(&mut self) {
        unsafe {
            let state = self.future.get_mut();
            if !state.dropped {
                ManuallyDrop::drop(&mut state.future);
            }
        }
    }
}

struct TaskFutureState<F> {
    future: ManuallyDrop<F>,
    dropped: bool,
}

unsafe impl<F> Send for Task<F> where F: Send {}
unsafe impl<F> Sync for Task<F> where F: Send {}

impl ThreadPool {
    #[inline]
    pub fn new() -> io::Result<ThreadPool> {
        Self::builder().build()
    }

    #[inline]
    pub fn builder() -> Builder {
        Builder::new()
    }

    #[inline]
    pub fn handle(&self) -> Handle {
        Handle {
            shared: self.shared.clone(),
        }
    }

    #[inline]
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.shared.spawn(future)
    }

    #[inline]
    pub fn spawn_with_handle<F>(&self, future: F) -> RemoteHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (remote, remote_handle) = future.remote_handle();
        self.shared.spawn(remote);
        remote_handle
    }

    fn build(builder: &mut Builder) -> io::Result<Self> {
        let event_queue_factory = match builder.event_queue_factory.take() {
            Some(factory) => factory,
            None => EventQueue::default_factory()?,
        };
        let supports_completion_stealing = event_queue_factory.supports_completion_stealing();

        struct WorkerInit {
            worker_shared: Arc<WorkerShared>,
            future_queue: crossbeam_deque::Worker<WorkItem>,
            worker_event_queue: Option<EventQueue>,
        }

        let mut workers = Vec::new();
        let mut worker_inits = Vec::new();
        for _ in 0..builder.worker_count {
            let future_queue = crossbeam_deque::Worker::new_lifo();
            let mut worker_event_queue = if supports_completion_stealing {
                Some(event_queue_factory.new()?)
            } else {
                None
            };
            let worker = WorkerHandle {
                shared: Arc::new(WorkerShared {
                    sleeping: AtomicBool::new(false).into(),
                }),
                future_queue: future_queue.stealer(),
                worker_event_queue: worker_event_queue.as_ref().map(|x| x.handle()),
            };
            worker_inits.push(WorkerInit {
                worker_shared: worker.shared.clone(),
                future_queue,
                worker_event_queue,
            });
            workers.push(worker);
        }
        let shared = Arc::new(Shared {
            injector: crossbeam_deque::Injector::new(),
            workers,
            shared_event_queue: if !supports_completion_stealing {
                Some(event_queue_factory.new()?)
            } else {
                None
            },
            sleep: Sleep::new(builder.worker_count).into(),
        });

        let mut worker_join_handles = Vec::new();
        for (worker_index, worker_init) in worker_inits.into_iter().enumerate() {
            let worker = Worker {
                shared: shared.clone(),
                worker_shared: worker_init.worker_shared,
                future_queue: worker_init.future_queue,
                worker_event_queue: worker_init.worker_event_queue,
            };
            let join_handle = std::thread::Builder::new()
                .name((builder.thread_name)(worker_index))
                .spawn({
                    let wrap_start = builder.wrap_start.clone();
                    move || worker.run(worker_index, wrap_start)
                })?;
            worker_join_handles.push(join_handle);
        }
        Ok(ThreadPool {
            shared,
            worker_join_handles,
        })
    }
}

impl Handle {
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.shared.spawn(future)
    }

    pub fn spawn_handle<F>(&self, future: F) -> RemoteHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let (remote, remote_handle) = future.remote_handle();
        self.shared.spawn(remote);
        remote_handle
    }
}

thread_local! {
    static SMALL_RNG: RefCell<SmallRng> = SmallRng::from_entropy().into();
}

impl Shared {
    fn spawn<F>(self: &Arc<Self>, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.injector.push(WorkItem {
            task: Arc::new(Task {
                pool: self.clone(),
                future: CachePadded::new(UnsafeCell::new(TaskFutureState {
                    future: ManuallyDrop::new(future),
                    dropped: false,
                })),
                wake_count: CachePadded::new(AtomicUsize::new(1)),
            }),
        });
        // FIXME: don't send wake if at least one thread is in a state where it might steal from the injector
        let worker = SMALL_RNG.with(|rng| {
            let mut rng = rng.borrow_mut();
            rand::distributions::Slice::new(&self.workers)
                .unwrap()
                .sample(&mut *rng)
        });
        if let Some(worker_event_queue) = &worker.worker_event_queue {
            worker_event_queue.wake().expect("failed to queue wakeup event")
        } else {
            let event_queue = self.shared_event_queue.as_ref().unwrap();
            event_queue.wake().expect("failed to queue wakeup event");
        }
    }
}

impl Worker {
    fn run(self, index: usize, wrap_start: Option<Arc<dyn Fn(usize, Box<dyn FnOnce()>) + Send + Sync + 'static>>) {
        match panic::catch_unwind(panic::AssertUnwindSafe(move || {
            if let Some(wrap_start) = wrap_start {
                wrap_start(index, Box::new(move || self.do_run(index)))
            } else {
                self.do_run(index)
            }
        })) {
            Ok(()) => {}
            Err(_) => {
                eprintln!("aborting due to unhandled panic in worker thread");
                std::process::abort()
            }
        }
    }

    fn do_run(mut self, my_index: usize) {
        // FIXME: this is aliasing
        unsafe {
            CURRENT_THREAD_WORKER = Some(NonNull::new_unchecked(&self as *const Worker as *mut Worker));
        }

        let mut workers_shuffled: Vec<_> = (0..self.shared.workers.len()).filter(|&x| x != my_index).collect();

        'get_work: loop {
            {
                let _guard = crate::spawn::set_executor(&self.shared);
                let _guard = if let Some(worker_event_queue) = &mut self.worker_event_queue {
                    Ok(worker_event_queue.set_current_mut())
                } else {
                    Err(self.shared.shared_event_queue.as_ref().unwrap().set_current())
                };
                while let Some(work_item) = self.future_queue.pop() {
                    unsafe {
                        work_item.task.run();
                    }
                }
            }

            let mut idle_state = self.shared.sleep.start_looking(my_index);
            'steal_work: loop {
                'steal_injector: loop {
                    match self.shared.injector.steal() {
                        crossbeam_deque::Steal::Empty => break 'steal_injector,
                        crossbeam_deque::Steal::Success(item) => {
                            wake_if_needed(
                                &self.shared,
                                &mut workers_shuffled,
                                self.shared.sleep.work_found(idle_state),
                            );
                            self.future_queue.push(item);
                            continue 'get_work;
                        }
                        crossbeam_deque::Steal::Retry => continue 'steal_injector,
                    }
                }

                // Poll the event queue without sleeping first
                let events_pulled = if let Some(event_queue) = &mut self.worker_event_queue {
                    event_queue
                        .poll_mut(Some(Duration::ZERO))
                        .expect("polling event queue failed")
                } else {
                    let event_queue = self.shared.shared_event_queue.as_ref().unwrap();
                    event_queue
                        .poll(Some(Duration::ZERO))
                        .expect("polling event queue failed")
                };
                if events_pulled > 0 {
                    wake_if_needed(
                        &self.shared,
                        &mut workers_shuffled,
                        self.shared.sleep.work_found(idle_state),
                    );
                    continue 'get_work;
                }

                'steal_other_threads: loop {
                    SMALL_RNG.with(|rng| {
                        let mut rng = rng.borrow_mut();
                        workers_shuffled.shuffle(&mut *rng);
                    });

                    'steal_scan: for &i in &workers_shuffled {
                        let worker = &self.shared.workers[i];
                        'steal_one: for _ in 0..2 {
                            match worker.future_queue.steal() {
                                crossbeam_deque::Steal::Empty => continue 'steal_scan,
                                crossbeam_deque::Steal::Success(item) => {
                                    wake_if_needed(
                                        &self.shared,
                                        &mut workers_shuffled,
                                        self.shared.sleep.work_found(idle_state),
                                    );
                                    self.future_queue.push(item);
                                    continue 'get_work;
                                }
                                crossbeam_deque::Steal::Retry => continue 'steal_one,
                            }
                        }
                    }

                    break 'steal_other_threads;
                }

                if let Some(sleep_state) = self.shared.sleep.no_work_found(&mut idle_state) {
                    self.worker_shared.sleeping.store(true, Ordering::SeqCst);
                    // Now we really don't have anything to do, wait until somebody wakes us
                    if let Some(event_queue) = &mut self.worker_event_queue {
                        event_queue.poll_mut(None).expect("polling event queue failed")
                    } else {
                        let event_queue = self.shared.shared_event_queue.as_ref().unwrap();
                        event_queue.poll(None).expect("polling event queue failed")
                    };
                    if self.worker_shared.sleeping.swap(false, Ordering::SeqCst) {
                        // We were woken due to work on our event queue, not by another thread. So it's our responsiblity to
                        // update the counters
                        self.shared.sleep.woke_self();
                    }
                    continue 'get_work;
                } else {
                    continue 'steal_work;
                }
            }
        }

        unsafe {
            // Ensure that if any destructors of our work items ccess CURRENT_THREAD_WORKER it doesn't point to the destroying object
            CURRENT_THREAD_WORKER = None;
        }
    }
}

fn wake_if_needed(shared: &Arc<Shared>, workers_shuffled: &mut Vec<usize>, instructions: Option<WakeInstrunctions>) {
    let Some(instructions) = instructions else {
        return;
    };
    SMALL_RNG.with(|rng| {
        let mut rng = rng.borrow_mut();
        workers_shuffled.shuffle(&mut *rng);
    });

    let mut remaining_wake_count = instructions.threads_to_wake;
    for &thread_index in workers_shuffled.iter() {
        if remaining_wake_count < 1 {
            break;
        }
        let thread = &shared.workers[thread_index];
        if thread.try_wake(shared) {
            remaining_wake_count -= 1;
        }
    }
}

impl WorkerHandle {
    fn try_wake(&self, shared: &Arc<Shared>) -> bool {
        let was_asleep = self.shared.sleeping.swap(false, Ordering::SeqCst);
        if !was_asleep {
            return false;
        }
        shared.sleep.will_wake_thread();
        if let Some(event_queue) = &self.worker_event_queue {
            event_queue.wake().unwrap();
        } else {
            todo!()
        }
        true
    }
}

unsafe fn submit_work(work: WorkItem) {
    if let Some(worker) = CURRENT_THREAD_WORKER.map(|x| x.as_ref()) {
        worker.future_queue.push(work);
        return;
    }

    // TODO: think about whether we can avoid this copy
    let pool = work.task.pool();
    pool.injector.push(work);
    // FIXME: don't send wake if at least one thread is in a state where it might steal from the injector
    let worker = SMALL_RNG.with(|rng| {
        let mut rng = rng.borrow_mut();
        rand::distributions::Slice::new(&pool.workers)
            .unwrap()
            .sample(&mut *rng)
    });
    if let Some(worker_event_queue) = &worker.worker_event_queue {
        worker_event_queue.wake().expect("failed to queue wakeup event")
    } else {
        let event_queue = pool.shared_event_queue.as_ref().unwrap();
        event_queue.wake().expect("failed to queue wakeup event");
    }
}

#[thread_local]
static mut CURRENT_THREAD_WORKER: Option<NonNull<Worker>> = None;

impl<F> GenTask for Task<F>
where
    F: Future<Output = ()> + Send + 'static,
{
    unsafe fn run(self: Arc<Self>) {
        let mut future_state = &mut *self.future.get();
        let mut pinned = unsafe { Pin::new_unchecked(&mut *future_state.future) };
        let mut init_count = self.wake_count.load(Ordering::Acquire);
        let self_ptr: *const Self = &*self;
        let waker: Waker = self.into();
        let mut context = Context::from_waker(&waker);
        loop {
            match pinned.as_mut().poll(&mut context) {
                Poll::Ready(()) => {
                    // The future can now be released even if there are existing wakers pointing to it since it will
                    // never wake up again.
                    // NOTE: it's important we set this first, in case drop panics
                    future_state.dropped = true;
                    ManuallyDrop::drop(&mut future_state.future);
                    break;
                }
                Poll::Pending => {
                    match unsafe { &*self_ptr }.wake_count.compare_exchange(
                        init_count,
                        0,
                        Ordering::AcqRel,
                        Ordering::Acquire,
                    ) {
                        Ok(_) => break,
                        Err(new_count) => {
                            // There was a wake while we were polling, we need to poll again to resolve the race
                            init_count = new_count;
                        }
                    }
                }
            }
        }
    }

    fn pool(&self) -> Arc<Shared> {
        self.pool.clone()
    }
}

impl<F> Wake for Task<F>
where
    F: Future<Output = ()> + Send + 'static,
{
    fn wake_by_ref(self: &Arc<Self>) {
        let orig_count = self.wake_count.fetch_add(1, Ordering::AcqRel);
        if orig_count == 0 {
            // Task was sleeping, resubmit it
            unsafe { submit_work(WorkItem { task: self.clone() }) }
        }
    }

    fn wake(self: Arc<Self>) {
        let orig_count = self.wake_count.fetch_add(1, Ordering::AcqRel);
        if orig_count == 0 {
            // Task was sleeping, resubmit it
            unsafe { submit_work(WorkItem { task: self }) }
        }
    }
}

impl crate::spawn::Executor for Arc<Shared> {
    fn spawn(&self, future: future::BoxFuture<'static, ()>) {
        Shared::spawn(self, future)
    }

    fn wake_after(&self, deadline: std::time::Instant, cx: &mut std::task::Context) {
        todo!()
    }
}
