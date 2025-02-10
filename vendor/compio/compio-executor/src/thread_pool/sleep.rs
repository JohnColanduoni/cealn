use std::sync::atomic::{AtomicU64, Ordering};

pub(super) struct Sleep {
    counter: AtomicU64,
}

pub(super) struct IdleState {
    worker_index: usize,
    rounds: u32,
}

pub(super) struct SleepState {}

pub(super) struct WakeInstrunctions {
    pub threads_to_wake: usize,
}

const ROUNDS_UNTIL_SLEEPING: u32 = 32;

const THREADS_BITS: usize = 16;
const THREAD_BITS_MASK: u64 = (1 << THREADS_BITS) - 1;
const SLEEPING_SHIFT: usize = 0 * THREADS_BITS;
const INACTIVE_SHIFT: usize = 1 * THREADS_BITS;

const ONE_SLEEPING: u64 = 1 << SLEEPING_SHIFT;
const ONE_INACTIVE: u64 = 1 << INACTIVE_SHIFT;

impl Sleep {
    pub fn new(thread_count: usize) -> Self {
        Sleep {
            counter: AtomicU64::new(0),
        }
    }

    #[inline]
    pub(super) fn start_looking(&self, worker_index: usize) -> IdleState {
        self.add_inactive_thread();

        IdleState {
            worker_index,
            rounds: 0,
        }
    }

    #[inline]
    pub(super) fn work_found(&self, idle_state: IdleState) -> Option<WakeInstrunctions> {
        let threads_to_wake = self.sub_inactive_thread();
        if threads_to_wake > 0 {
            Some(WakeInstrunctions { threads_to_wake })
        } else {
            None
        }
    }

    #[inline]
    pub(super) fn no_work_found(&self, idle_state: &mut IdleState) -> Option<SleepState> {
        if idle_state.rounds < ROUNDS_UNTIL_SLEEPING {
            idle_state.rounds += 1;
            None
        } else {
            let old_counters = self.counter.load(Ordering::SeqCst);
            let mut new_counters = old_counters;
            new_counters -= ONE_INACTIVE;
            new_counters += ONE_SLEEPING;
            match self
                .counter
                .compare_exchange_weak(old_counters, new_counters, Ordering::AcqRel, Ordering::Relaxed)
            {
                Ok(_) => Some(SleepState {}),
                Err(_) => {
                    // Contention on the counter indicates activity on the thread pool. Execute another loop
                    None
                }
            }
        }
    }

    #[inline]
    pub(super) fn will_wake_thread(&self) {
        self.sub_sleeping_thread();
    }

    #[inline]
    pub(super) fn woke_self(&self) {
        self.sub_sleeping_thread();
    }

    #[inline]
    fn add_inactive_thread(&self) {
        self.counter.fetch_add(ONE_INACTIVE, Ordering::SeqCst);
    }

    #[inline]
    fn sub_inactive_thread(&self) -> usize {
        let old_value = self.counter.fetch_sub(ONE_INACTIVE, Ordering::SeqCst);
        let sleeping_threads = (old_value >> SLEEPING_SHIFT) & THREAD_BITS_MASK;
        std::cmp::min(sleeping_threads as usize, 2)
    }

    #[inline]
    fn sub_sleeping_thread(&self) {
        self.counter.fetch_sub(ONE_SLEEPING, Ordering::SeqCst);
    }
}
