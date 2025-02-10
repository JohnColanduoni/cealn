use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    io,
    rc::Rc,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    task::{Context, Poll, Wake, Waker},
    time::Instant,
};

use futures::{
    future::{FutureObj, LocalFutureObj},
    pin_mut,
    prelude::*,
    stream::FuturesUnordered,
    task::ArcWake,
};

use compio_core::{
    event_queue::{CustomEvent, Handle},
    EventQueue,
};

use crate::spawn::Executor;

pub struct LocalPool {
    id: usize,
    event_queue: EventQueue,
    futures: FuturesUnordered<LocalFutureObj<'static, ()>>,
    // FIXME: this is trash
    timers: BTreeMap<Instant, Waker>,
    shared: Rc<Shared>,
    sync_shared: Option<Arc<SyncShared>>,
}

pub struct LocalSpawner {
    shared: Rc<Shared>,
}

pub struct Spawner {
    shared: Arc<SyncShared>,
}

struct Shared {
    incoming: RefCell<Vec<LocalFutureObj<'static, ()>>>,
    incoming_timers: RefCell<Vec<(Instant, Waker)>>,
    waker: Arc<PoolWaker>,
}

struct SyncShared {
    incoming: Mutex<Vec<FutureObj<'static, ()>>>,
    incoming_timers: Mutex<Vec<(Instant, Waker)>>,
    waker: Arc<PoolWaker>,
}

impl LocalPool {
    pub fn new() -> io::Result<LocalPool> {
        let event_queue = EventQueue::new()?;
        Ok(Self::with_event_queue(event_queue))
    }

    pub fn with_event_queue(mut event_queue: EventQueue) -> LocalPool {
        // The ordering here can be relaxed because we only care about uniqueness, not ordering
        let id = NEXT_LOCAL_POOL_ID.fetch_add(1, Ordering::Relaxed);
        let remote_wake_event = event_queue.new_custom_event(trigger_wake);
        let waker = Arc::new(PoolWaker {
            pool_id: id,
            handle: event_queue.handle(),
            remote_wake_event,
            // Start in an awakened state
            signal_level: AtomicUsize::new(1),
        });
        let shared = Rc::new(Shared {
            incoming: Default::default(),
            incoming_timers: Default::default(),
            waker,
        });
        LocalPool {
            id,
            event_queue,
            futures: Default::default(),
            timers: Default::default(),
            shared,
            sync_shared: None,
        }
    }

    pub fn spawner(&self) -> LocalSpawner {
        LocalSpawner {
            shared: self.shared.clone(),
        }
    }

    pub fn remote_spawner(&mut self) -> Spawner {
        let shared = match &self.sync_shared {
            Some(shared) => shared.clone(),
            None => {
                let shared = Arc::new(SyncShared {
                    incoming: Default::default(),
                    incoming_timers: Default::default(),
                    waker: self.shared.waker.clone(),
                });
                self.sync_shared = Some(shared.clone());
                shared
            }
        };
        Spawner { shared }
    }

    pub fn run_until<F>(&mut self, future: F) -> F::Output
    where
        F: Future,
    {
        let _guard = CurrentLocalPool::enter(self.id);

        let event_queue = &mut self.event_queue;
        let futures = &mut self.futures;
        let timers = &mut self.timers;
        let waker = &self.shared.waker;
        let incoming = &self.shared.incoming;
        let incoming_timers = &self.shared.incoming_timers;
        let spawner = LocalSpawner {
            shared: self.shared.clone(),
        };
        let _executor_guard = crate::spawn::set_executor(&spawner);
        let _event_queue_guard = event_queue.set_current_mut();
        let pool_waker = futures::task::waker_ref(waker);
        let mut context = Context::from_waker(&pool_waker);
        pin_mut!(future);
        let mut first_iteration = true;
        loop {
            // TODO: weaken?
            let mut init_signal_level = waker.signal_level.load(Ordering::SeqCst);
            loop {
                // Always poll our future the first time to satisfy contract
                if init_signal_level > 0 || first_iteration {
                    match future.as_mut().poll(&mut context) {
                        Poll::Ready(value) => {
                            return value;
                        }
                        Poll::Pending => {}
                    }
                }
                if init_signal_level > 0 {
                    {
                        // Grab any spawned futures and add them to the pool
                        // This must be done in its own scope because polling the pool may result in an attempt to
                        // spawn a future, which would cause a borrow violation.
                        let mut incoming = incoming.borrow_mut();
                        futures.extend(incoming.drain(..));
                        let mut incoming_timers = incoming_timers.borrow_mut();
                        timers.extend(incoming_timers.drain(..));

                        if let Some(sync_shared) = &self.sync_shared {
                            let mut incoming = sync_shared.incoming.lock().unwrap();
                            futures.extend(incoming.drain(..).map(LocalFutureObj::from));
                            let mut incoming_timers = sync_shared.incoming_timers.lock().unwrap();
                            timers.extend(incoming_timers.drain(..));
                        }
                    }

                    while let Poll::Ready(Some(())) = futures.poll_next_unpin(&mut context) {}
                }
                first_iteration = false;

                // Try to reset signal level to 0, iff it hasn't changed. Otherwise we had a wakeup while we were
                // polling and need to poll again to handle a possible race.
                // TODO: weaken?
                match waker
                    .signal_level
                    .compare_exchange(init_signal_level, 0, Ordering::SeqCst, Ordering::SeqCst)
                {
                    Ok(_) => break,
                    Err(new_signal_level) => {
                        // Update our expected baseline signal level and go again
                        init_signal_level = new_signal_level;
                    }
                }
            }

            // Handle timeouts
            let mut timeout_deadline = None;
            {
                let now = Instant::now();
                let mut woke_timers = false;
                while let Some(lowest) = timers.first_entry() {
                    if lowest.key() < &now {
                        lowest.remove().wake();
                        woke_timers = true;
                    } else {
                        timeout_deadline = Some(*lowest.key());
                        break;
                    }
                }
                if woke_timers {
                    // We triggered some timers, double check if we woke ourself
                    continue;
                }
            }

            // If we get to this point, the signal level is zero. It's important we don't call any code that might
            // trigger a wake here except for `EventQueue::poll` as it would then get lost (we won't check the signal
            // level again until after `poll` returns).

            // Now wait for a wakeup while polling the event queue
            EventQueue::with_current_mut(|event_queue| {
                loop {
                    let timeout = if let Some(deadline) = timeout_deadline {
                        let now = Instant::now();
                        match deadline.checked_duration_since(now) {
                            Some(timeout) => Some(timeout),
                            None => break,
                        }
                    } else {
                        None
                    };
                    event_queue.poll_mut(timeout).expect("polling event queue failed");
                    // TODO: weaken?
                    if waker.signal_level.load(Ordering::SeqCst) != 0 {
                        break;
                    }
                }
            });
        }
    }
}

impl LocalSpawner {
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + 'static,
    {
        self.shared.incoming.borrow_mut().push(future.boxed_local().into());
        ArcWake::wake_by_ref(&self.shared.waker);
    }

    pub fn spawn_boxed(&self, future: future::BoxFuture<'static, ()>) {
        self.shared
            .incoming
            .borrow_mut()
            .push(LocalFutureObj::from(FutureObj::from(future)));
        ArcWake::wake_by_ref(&self.shared.waker);
    }
}

impl Executor for LocalSpawner {
    fn spawn(&self, future: future::BoxFuture<'static, ()>) {
        LocalSpawner::spawn_boxed(self, future)
    }

    fn wake_after(&self, deadline: std::time::Instant, cx: &mut std::task::Context) {
        self.shared
            .incoming_timers
            .borrow_mut()
            .push((deadline, cx.waker().clone()));
        ArcWake::wake_by_ref(&self.shared.waker);
    }
}

impl Spawner {
    pub fn spawn<F>(&self, future: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.shared.incoming.lock().unwrap().push(future.boxed().into());
        ArcWake::wake_by_ref(&self.shared.waker);
    }

    pub fn spawn_boxed(&self, future: future::BoxFuture<'static, ()>) {
        self.shared.incoming.lock().unwrap().push(FutureObj::from(future));
        ArcWake::wake_by_ref(&self.shared.waker);
    }
}

impl Executor for Spawner {
    fn spawn(&self, future: future::BoxFuture<'static, ()>) {
        Spawner::spawn_boxed(self, future)
    }

    fn wake_after(&self, deadline: std::time::Instant, cx: &mut std::task::Context) {
        todo!()
    }
}

fn trigger_wake(_data: usize) {
    // This doesn't need to do anything, since it'll cause `poll` to return and the loop to run again
}

struct PoolWaker {
    pool_id: usize,
    handle: Handle,
    remote_wake_event: CustomEvent,

    // Zero indicates there is no pending wake. Non-zero indicates a wake has been enqueued.
    // Wakers do an atomic increment. If the previous value was zero, they schedule a wake
    // in one of the following ways:
    //  * If the pool is currently polling on the waking thread, they do nothing. The change in signal level will be
    //    observed and trigger another poll.
    //  * If the pool is not currently polling or the wake is being triggered from another thread, a noop callback is
    //    enqueued on the `EventQueue`. This will cause the event queue to wake up and perform a poll cycle.
    signal_level: AtomicUsize,
}

impl ArcWake for PoolWaker {
    fn wake_by_ref(arc_self: &Arc<Self>) {
        // TODO: Can this be weaker? Not sure what the memory ordering guarantees of `enqueue_custom_event` should be.
        //       We can probably use logic similar to decrementing reference counts either way, where we use a weak
        //       ordering and put an explicit barrier if the previous value was zero and we take an action.
        let previous_signal_level = arc_self.signal_level.fetch_add(1, Ordering::AcqRel);
        if previous_signal_level == 0 {
            let current_pool_id = POLLING_LOCAL_POOL_ID.with(|x| x.get());
            if current_pool_id == Some(arc_self.pool_id) {
                // No need to do anything, next pool cycle will detect incremented signal level
            } else {
                arc_self
                    .handle
                    .enqueue_custom_event(&arc_self.remote_wake_event, 0)
                    .expect("failed to enqueue wakeup event");
            }
        }
    }
}

static NEXT_LOCAL_POOL_ID: AtomicUsize = AtomicUsize::new(1);

thread_local! {
    static POLLING_LOCAL_POOL_ID: Cell<Option<usize>> = Cell::new(None);
}

struct CurrentLocalPool {
    _general_guard: futures::executor::Enter,
}

impl Drop for CurrentLocalPool {
    fn drop(&mut self) {
        POLLING_LOCAL_POOL_ID.with(|x| x.set(None));
    }
}

impl CurrentLocalPool {
    pub fn enter(id: usize) -> CurrentLocalPool {
        let _general_guard = futures::executor::enter().expect("recursive entry into future executor");
        POLLING_LOCAL_POOL_ID.with(|x| {
            // This check should be handled by the global recursive executor guard above
            debug_assert!(x.get().is_none());
            x.set(Some(id))
        });

        CurrentLocalPool { _general_guard }
    }
}
