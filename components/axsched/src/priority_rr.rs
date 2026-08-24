use alloc::sync::Arc;
use core::{
    ops::Deref,
    sync::atomic::{AtomicIsize, AtomicU64, Ordering},
};

use ax_linked_list_r4l::{GetLinks, Links, List};

use crate::{BaseScheduler, MAX_PRIORITY, MIN_PRIORITY, SchedPriority};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PriorityRRStats {
    pub quantum_expiries: u64,
    pub same_priority_rotations: u64,
    pub slice_preserving_preemptions: u64,
    pub voluntary_requeues: u64,
    /// Timer ticks skipped while no peer at the current priority was runnable.
    pub idle_quantum_skips: u64,
    /// Forced service windows granted to a lower-priority runnable task.
    pub lower_priority_services: u64,
}

#[repr(align(64))]
struct CacheAlignedCounter(AtomicU64);

static QUANTUM_EXPIRIES: CacheAlignedCounter = CacheAlignedCounter(AtomicU64::new(0));
static SAME_PRIORITY_ROTATIONS: CacheAlignedCounter = CacheAlignedCounter(AtomicU64::new(0));
static SLICE_PRESERVING_PREEMPTIONS: CacheAlignedCounter = CacheAlignedCounter(AtomicU64::new(0));
static VOLUNTARY_REQUEUES: CacheAlignedCounter = CacheAlignedCounter(AtomicU64::new(0));
static IDLE_QUANTUM_SKIPS: CacheAlignedCounter = CacheAlignedCounter(AtomicU64::new(0));
static LOWER_PRIORITY_SERVICES: CacheAlignedCounter = CacheAlignedCounter(AtomicU64::new(0));

/// Returns aggregate fixed-priority round-robin mechanism counters.
pub fn priority_rr_stats_snapshot() -> PriorityRRStats {
    PriorityRRStats {
        quantum_expiries: QUANTUM_EXPIRIES.0.load(Ordering::Relaxed),
        same_priority_rotations: SAME_PRIORITY_ROTATIONS.0.load(Ordering::Relaxed),
        slice_preserving_preemptions: SLICE_PRESERVING_PREEMPTIONS.0.load(Ordering::Relaxed),
        voluntary_requeues: VOLUNTARY_REQUEUES.0.load(Ordering::Relaxed),
        idle_quantum_skips: IDLE_QUANTUM_SKIPS.0.load(Ordering::Relaxed),
        lower_priority_services: LOWER_PRIORITY_SERVICES.0.load(Ordering::Relaxed),
    }
}

/// Task wrapper for fixed-priority round-robin scheduling.
pub struct PriorityRRTask<T, const MAX_TIME_SLICE: usize> {
    inner: T,
    time_slice: AtomicIsize,
    forced_service_ticks: AtomicIsize,
    links: Links<Self>,
}

impl<T, const S: usize> PriorityRRTask<T, S> {
    /// Creates a task with a full time slice.
    pub const fn new(inner: T) -> Self {
        Self {
            inner,
            time_slice: AtomicIsize::new(S as isize),
            forced_service_ticks: AtomicIsize::new(0),
            links: Links::new(),
        }
    }

    fn time_slice(&self) -> isize {
        self.time_slice.load(Ordering::Acquire)
    }

    fn reset_time_slice(&self) {
        self.time_slice.store(S as isize, Ordering::Release);
    }

    fn begin_forced_service(&self) {
        self.forced_service_ticks
            .store(LOWER_SERVICE_BUDGET_TICKS as isize, Ordering::Release);
    }

    fn has_forced_service(&self) -> bool {
        self.forced_service_ticks.load(Ordering::Acquire) > 0
    }

    fn consume_forced_service_tick(&self) -> bool {
        self.forced_service_ticks.fetch_sub(1, Ordering::AcqRel) <= 1
    }

    fn end_forced_service(&self) {
        self.forced_service_ticks.store(0, Ordering::Release);
    }

    /// Returns a reference to the wrapped task.
    pub const fn inner(&self) -> &T {
        &self.inner
    }
}

impl<T, const S: usize> GetLinks for PriorityRRTask<T, S> {
    type EntryType = Self;

    fn get_links(data: &Self::EntryType) -> &Links<Self::EntryType> {
        &data.links
    }
}

impl<T, const S: usize> Deref for PriorityRRTask<T, S> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl<T: SchedPriority, const S: usize> SchedPriority for PriorityRRTask<T, S> {
    fn sched_priority(&self) -> isize {
        self.inner.sched_priority()
    }

    fn set_sched_priority(&self, priority: isize) {
        self.inner.set_sched_priority(priority);
    }
}

/// Strict fixed priority between levels, with bounded round-robin within a
/// priority level.
pub struct PriorityRRScheduler<T: SchedPriority, const MAX_TIME_SLICE: usize> {
    ready_queues:
        [List<Arc<PriorityRRTask<T, MAX_TIME_SLICE>>>; (MAX_PRIORITY - MIN_PRIORITY + 1) as usize],
    /// Waiting age, in scheduler ticks, for each runnable priority queue.
    wait_age: [u16; (MAX_PRIORITY - MIN_PRIORITY + 1) as usize],
    /// Priority currently receiving a bounded starvation-prevention window.
    forced_service_priority: Option<usize>,
}

// A runnable high-priority guest must remain dominant, but it must not be able
// to starve a lower-priority guest indefinitely. The host scheduler tick is
// 10 ms (TICKS_PER_SEC = 100), so this bounds lower-priority dispatch delay to
// roughly 200 ms of accumulated higher-priority runnable time plus one service
// tick. The bound is independent of workload duration.
const LOWER_SERVICE_INTERVAL_TICKS: u16 = 20;
// One complete host tick is enough for a vCPU to process accumulated timer
// work. This window survives VM-exit yields and wakeup preemptions, but remains
// tightly bounded so the higher-priority RT guest regains the CPU promptly.
const LOWER_SERVICE_BUDGET_TICKS: u16 = 1;

impl<T: SchedPriority, const S: usize> PriorityRRScheduler<T, S> {
    /// Creates an empty scheduler.
    pub const fn new() -> Self {
        Self {
            ready_queues: [const { List::new() }; (MAX_PRIORITY - MIN_PRIORITY + 1) as usize],
            wait_age: [0; (MAX_PRIORITY - MIN_PRIORITY + 1) as usize],
            forced_service_priority: None,
        }
    }

    /// Returns the scheduler name used by boot diagnostics.
    pub const fn scheduler_name() -> &'static str {
        "Fixed-priority round-robin"
    }

    fn priority_index(task: &PriorityRRTask<T, S>) -> usize {
        task.sched_priority().clamp(MIN_PRIORITY, MAX_PRIORITY) as usize
    }
}

impl<T: SchedPriority, const S: usize> BaseScheduler for PriorityRRScheduler<T, S> {
    type SchedItem = Arc<PriorityRRTask<T, S>>;

    fn init(&mut self) {}

    fn add_task(&mut self, task: Self::SchedItem) {
        self.ready_queues[Self::priority_index(&task)].push_back(task);
    }

    fn remove_task(&mut self, task: &Self::SchedItem) -> Option<Self::SchedItem> {
        unsafe { self.ready_queues[Self::priority_index(task)].remove(task) }
    }

    fn pick_next_task(&mut self) -> Option<Self::SchedItem> {
        if let Some(priority) = self.forced_service_priority {
            if let Some(task) = self.ready_queues[priority].pop_front() {
                return Some(task);
            }
            // The protected task blocked or exited instead of requeueing.
            self.forced_service_priority = None;
        }
        if let Some(priority) = self
            .wait_age
            .iter()
            .position(|age| *age >= LOWER_SERVICE_INTERVAL_TICKS)
        {
            if let Some(task) = self.ready_queues[priority].pop_front() {
                self.wait_age[priority] = 0;
                task.begin_forced_service();
                self.forced_service_priority = Some(priority);
                LOWER_PRIORITY_SERVICES.0.fetch_add(1, Ordering::Relaxed);
                return Some(task);
            }
        }
        let task = self.ready_queues.iter_mut().rev().find_map(List::pop_front);
        if let Some(ref task) = task {
            self.wait_age[Self::priority_index(task)] = 0;
        }
        task
    }

    fn put_prev_task(&mut self, prev: Self::SchedItem, preempt: bool) {
        let priority = Self::priority_index(&prev);
        self.wait_age[priority] = self.wait_age[priority].min(1);
        let queue = &mut self.ready_queues[priority];
        // A higher-priority preemption preserves the current task's position
        // and remaining slice. Quantum expiry and voluntary yield rotate the
        // task to the tail and start a fresh slice.
        if prev.has_forced_service() && self.forced_service_priority == Some(priority) {
            // Preserve the service window across both wakeup preemption and
            // the vCPU loop's cooperative VM-exit yield.
            queue.push_front(prev);
        } else if preempt && prev.time_slice() > 0 {
            SLICE_PRESERVING_PREEMPTIONS
                .0
                .fetch_add(1, Ordering::Relaxed);
            queue.push_front(prev);
        } else {
            if preempt && !queue.is_empty() {
                SAME_PRIORITY_ROTATIONS.0.fetch_add(1, Ordering::Relaxed);
            } else if !preempt {
                VOLUNTARY_REQUEUES.0.fetch_add(1, Ordering::Relaxed);
            }
            prev.reset_time_slice();
            queue.push_back(prev);
        }
    }

    fn task_tick(&mut self, current: &Self::SchedItem) -> bool {
        // A quantum is a fairness budget, not a periodic forced yield.  When
        // no peer at this priority is runnable, expiring the budget would only
        // requeue and immediately select the same task (or briefly bounce to a
        // lower-priority task).  Keep the task running and replenish its
        // budget; once a same-priority peer appears, normal bounded RR starts.
        let current_priority = Self::priority_index(current);
        if self.forced_service_priority == Some(current_priority) && current.has_forced_service() {
            if current.consume_forced_service_tick() {
                current.end_forced_service();
                self.forced_service_priority = None;
                return true;
            }
            return false;
        }
        for (priority, queue) in self.ready_queues.iter().enumerate() {
            if priority != current_priority && !queue.is_empty() {
                self.wait_age[priority] = self.wait_age[priority]
                    .saturating_add(1)
                    .min(LOWER_SERVICE_INTERVAL_TICKS);
            }
        }
        if self
            .wait_age
            .iter()
            .any(|age| *age >= LOWER_SERVICE_INTERVAL_TICKS)
        {
            return true;
        }
        if self.ready_queues[current_priority].is_empty() {
            current.reset_time_slice();
            IDLE_QUANTUM_SKIPS.0.fetch_add(1, Ordering::Relaxed);
            // A higher-priority task may have become runnable since the last
            // scheduling point. Preserve the current task's slice, but still
            // request the normal priority preemption.
            return current_priority + 1 < self.ready_queues.len()
                && self.ready_queues[current_priority + 1..]
                    .iter()
                    .any(|queue| !queue.is_empty());
        }
        let old_slice = current.time_slice.fetch_sub(1, Ordering::Release);
        let expired = old_slice <= 1;
        if expired {
            QUANTUM_EXPIRIES.0.fetch_add(1, Ordering::Relaxed);
        }
        expired
    }

    fn set_priority(&mut self, task: &Self::SchedItem, priority: isize) -> bool {
        if !(MIN_PRIORITY..=MAX_PRIORITY).contains(&priority) {
            return false;
        }
        task.set_sched_priority(priority);
        true
    }
}

impl<T: SchedPriority, const S: usize> Default for PriorityRRScheduler<T, S> {
    fn default() -> Self {
        Self::new()
    }
}
