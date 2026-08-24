//! AxVM-owned CPU-bucketed VM timer wheels.

#[cfg(test)]
use std::sync::{Mutex, MutexGuard};
#[cfg(test)]
use std::vec::Vec;
use std::{
    boxed::Box,
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::Duration,
};

use ax_std::os::arceos::{guard::PreemptGuard, modules::ax_task::IrqNotify, sync::IrqSafeMutex};
use ax_timer_list::{TimeValue, TimerEvent, TimerList};

#[cfg(not(test))]
use crate::host::{HostTime, default_host, task};

static TOKEN: AtomicUsize = AtomicUsize::new(0);
const TIMER_WORKER_STACK_SIZE: usize = 0x20_000;
#[cfg(feature = "timer-worker-priority-boost")]
const TIMER_WORKER_EVENT_BUDGET: usize = 1;
#[cfg(not(feature = "timer-worker-priority-boost"))]
const TIMER_WORKER_EVENT_BUDGET: usize = usize::MAX;
const NO_PUBLISHED_DEADLINE: u64 = 0;
const MAX_RT_STATS_CPUS: usize = 8;
const MAX_TIMER_CPUS: usize = usize::BITS as usize;
#[cfg(feature = "timer-latency-stats")]
const TIMER_EXPIRY_LATE_BUCKET_NS: u64 = 1_000;
#[cfg(feature = "timer-latency-stats")]
const TIMER_EXPIRY_LATE_BUCKETS: usize = 4_096;

static TIMER_REGISTER_COUNTS: [AtomicUsize; MAX_RT_STATS_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_RT_STATS_CPUS];
static TIMER_CANCEL_COUNTS: [AtomicUsize; MAX_RT_STATS_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_RT_STATS_CPUS];
static TIMER_EXPIRE_COUNTS: [AtomicUsize; MAX_RT_STATS_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_RT_STATS_CPUS];
static TIMER_WORKER_WAKE_COUNTS: [AtomicUsize; MAX_RT_STATS_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_RT_STATS_CPUS];
static TIMER_EXPIRY_BATCH_COUNTS: [AtomicUsize; MAX_RT_STATS_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_RT_STATS_CPUS];
#[cfg(feature = "timer-latency-stats")]
static TIMER_EXPIRY_LATE_HISTOGRAMS: [[AtomicUsize; TIMER_EXPIRY_LATE_BUCKETS]; MAX_RT_STATS_CPUS] =
    [const { [const { AtomicUsize::new(0) }; TIMER_EXPIRY_LATE_BUCKETS] }; MAX_RT_STATS_CPUS];
#[cfg(feature = "timer-latency-stats")]
static TIMER_EXPIRY_LATE_OVERFLOWS: [AtomicUsize; MAX_RT_STATS_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_RT_STATS_CPUS];
#[cfg(feature = "timer-latency-stats")]
static TIMER_EXPIRY_LATE_MAX_NS: [AtomicU64; MAX_RT_STATS_CPUS] =
    [const { AtomicU64::new(0) }; MAX_RT_STATS_CPUS];
static TIMER_LOCK_ACQUISITIONS: [AtomicUsize; MAX_RT_STATS_CPUS] =
    [const { AtomicUsize::new(0) }; MAX_RT_STATS_CPUS];
static TIMER_LOCK_WAIT_TOTAL_NS: [AtomicU64; MAX_RT_STATS_CPUS] =
    [const { AtomicU64::new(0) }; MAX_RT_STATS_CPUS];
static TIMER_LOCK_WAIT_MAX_NS: [AtomicU64; MAX_RT_STATS_CPUS] =
    [const { AtomicU64::new(0) }; MAX_RT_STATS_CPUS];
static TIMER_LOCK_HOLD_TOTAL_NS: [AtomicU64; MAX_RT_STATS_CPUS] =
    [const { AtomicU64::new(0) }; MAX_RT_STATS_CPUS];
static TIMER_LOCK_HOLD_MAX_NS: [AtomicU64; MAX_RT_STATS_CPUS] =
    [const { AtomicU64::new(0) }; MAX_RT_STATS_CPUS];

pub(crate) fn rt_timer_stats_snapshot() -> Vec<crate::TimerRuntimeCounts> {
    let snapshot_now_ns = current_host_time().as_nanos().min(u64::MAX as u128) as u64;
    (0..MAX_RT_STATS_CPUS)
        .map(|cpu_id| {
            let expiry_lateness = timer_expiry_lateness_snapshot(cpu_id);
            crate::TimerRuntimeCounts {
                cpu_id,
                snapshot_now_ns,
                wheel_next_deadline_ns: timer_wheel_next_deadline(cpu_id).map_or(0, |deadline| {
                    deadline.as_nanos().min(u64::MAX as u128) as u64
                }),
                published_deadline_ns: published_timer_deadline(cpu_id)
                    .deadline_nanos()
                    .unwrap_or(0),
                registered: TIMER_REGISTER_COUNTS[cpu_id].load(Ordering::Relaxed),
                cancelled: TIMER_CANCEL_COUNTS[cpu_id].load(Ordering::Relaxed),
                expired: TIMER_EXPIRE_COUNTS[cpu_id].load(Ordering::Relaxed),
                worker_wakes: TIMER_WORKER_WAKE_COUNTS[cpu_id].load(Ordering::Relaxed),
                expiry_batches: TIMER_EXPIRY_BATCH_COUNTS[cpu_id].load(Ordering::Relaxed),
                expiry_late_samples: expiry_lateness.samples,
                expiry_late_overflow: expiry_lateness.overflow,
                expiry_late_p50_ns: expiry_lateness.p50_ns,
                expiry_late_p99_ns: expiry_lateness.p99_ns,
                expiry_late_p99_9_ns: expiry_lateness.p99_9_ns,
                expiry_late_max_ns: expiry_lateness.max_ns,
                lock_acquisitions: TIMER_LOCK_ACQUISITIONS[cpu_id].load(Ordering::Relaxed),
                lock_wait_total_ns: TIMER_LOCK_WAIT_TOTAL_NS[cpu_id].load(Ordering::Relaxed),
                lock_wait_max_ns: TIMER_LOCK_WAIT_MAX_NS[cpu_id].load(Ordering::Relaxed),
                lock_hold_total_ns: TIMER_LOCK_HOLD_TOTAL_NS[cpu_id].load(Ordering::Relaxed),
                lock_hold_max_ns: TIMER_LOCK_HOLD_MAX_NS[cpu_id].load(Ordering::Relaxed),
            }
        })
        .collect()
}

#[derive(Clone, Copy, Default)]
struct TimerLatencySnapshot {
    samples: usize,
    overflow: usize,
    p50_ns: u64,
    p99_ns: u64,
    p99_9_ns: u64,
    max_ns: u64,
}

#[cfg(feature = "timer-latency-stats")]
fn timer_expiry_lateness_snapshot(cpu_id: usize) -> TimerLatencySnapshot {
    timer_latency_snapshot(
        &TIMER_EXPIRY_LATE_HISTOGRAMS[cpu_id],
        &TIMER_EXPIRY_LATE_OVERFLOWS[cpu_id],
        &TIMER_EXPIRY_LATE_MAX_NS[cpu_id],
    )
}

#[cfg(feature = "timer-latency-stats")]
fn timer_latency_snapshot(
    histogram: &[AtomicUsize; TIMER_EXPIRY_LATE_BUCKETS],
    overflow: &AtomicUsize,
    max_ns: &AtomicU64,
) -> TimerLatencySnapshot {
    let overflow = overflow.load(Ordering::Relaxed);
    let samples = histogram
        .iter()
        .map(|count| count.load(Ordering::Relaxed))
        .sum::<usize>()
        .saturating_add(overflow);
    let max_ns = max_ns.load(Ordering::Relaxed);
    TimerLatencySnapshot {
        samples,
        overflow,
        p50_ns: timer_expiry_lateness_percentile(histogram, samples, 50, 100, max_ns),
        p99_ns: timer_expiry_lateness_percentile(histogram, samples, 99, 100, max_ns),
        p99_9_ns: timer_expiry_lateness_percentile(histogram, samples, 999, 1_000, max_ns),
        max_ns,
    }
}

#[cfg(not(feature = "timer-latency-stats"))]
fn timer_expiry_lateness_snapshot(_cpu_id: usize) -> TimerLatencySnapshot {
    TimerLatencySnapshot::default()
}

#[cfg(feature = "timer-latency-stats")]
fn timer_expiry_lateness_percentile(
    histogram: &[AtomicUsize; TIMER_EXPIRY_LATE_BUCKETS],
    samples: usize,
    numerator: usize,
    denominator: usize,
    max_ns: u64,
) -> u64 {
    if samples == 0 {
        return 0;
    }
    let rank = samples
        .saturating_mul(numerator)
        .saturating_add(denominator - 1)
        / denominator;
    let mut cumulative = 0usize;
    for (bucket, count) in histogram.iter().enumerate() {
        cumulative = cumulative.saturating_add(count.load(Ordering::Relaxed));
        if cumulative >= rank {
            return ((bucket as u64) + 1).saturating_mul(TIMER_EXPIRY_LATE_BUCKET_NS);
        }
    }
    max_ns
}

#[cfg(feature = "timer-latency-stats")]
fn record_timer_expiry_lateness(cpu_id: usize, lateness_ns: u64) {
    record_timer_latency(
        cpu_id,
        &TIMER_EXPIRY_LATE_HISTOGRAMS,
        &TIMER_EXPIRY_LATE_OVERFLOWS,
        &TIMER_EXPIRY_LATE_MAX_NS,
        lateness_ns,
    );
}

#[cfg(feature = "timer-latency-stats")]
fn record_timer_latency(
    cpu_id: usize,
    histograms: &[[AtomicUsize; TIMER_EXPIRY_LATE_BUCKETS]; MAX_RT_STATS_CPUS],
    overflows: &[AtomicUsize; MAX_RT_STATS_CPUS],
    max_values: &[AtomicU64; MAX_RT_STATS_CPUS],
    latency_ns: u64,
) {
    let Some(histogram) = histograms.get(cpu_id) else {
        return;
    };
    max_values[cpu_id].fetch_max(latency_ns, Ordering::Relaxed);
    let bucket = (latency_ns / TIMER_EXPIRY_LATE_BUCKET_NS) as usize;
    if let Some(count) = histogram.get(bucket) {
        count.fetch_add(1, Ordering::Relaxed);
    } else {
        overflows[cpu_id].fetch_add(1, Ordering::Relaxed);
    }
}

#[cfg(not(feature = "timer-latency-stats"))]
fn record_timer_expiry_lateness(_cpu_id: usize, _lateness_ns: u64) {}

fn timer_wheel_next_deadline(cpu_id: usize) -> Option<TimeValue> {
    #[cfg(not(feature = "global-timer-wheel"))]
    {
        let timer_wheel = TIMER_WHEELS
            .get(cpu_id)
            .expect("AxVM timer CPU ID must fit the host CPU mask");
        timer_wheel.lock().next_deadline()
    }
    #[cfg(feature = "global-timer-wheel")]
    {
        TIMER_WHEELS
            .lock()
            .get(cpu_id)
            .expect("AxVM timer CPU ID must fit the host CPU mask")
            .next_deadline()
    }
}

/// Lock-free publication of one CPU's earliest AxVM timer deadline.
///
/// The host timer IRQ reads this value while selecting the next shared
/// hardware comparator deadline. AxVM wheel mutations publish before asking
/// the host timer arbiter to move the comparator earlier.
pub(crate) struct PublishedTimerDeadline {
    deadline_nanos: AtomicU64,
}

impl PublishedTimerDeadline {
    const fn new() -> Self {
        Self {
            deadline_nanos: AtomicU64::new(NO_PUBLISHED_DEADLINE),
        }
    }

    pub(crate) fn deadline_nanos(&self) -> Option<u64> {
        match self.deadline_nanos.load(Ordering::Acquire) {
            NO_PUBLISHED_DEADLINE => None,
            deadline => Some(deadline),
        }
    }

    fn publish(&self, deadline: Option<TimeValue>) {
        let deadline = deadline.map_or(NO_PUBLISHED_DEADLINE, |deadline| {
            (deadline.as_nanos().min(u64::MAX as u128) as u64).max(1)
        });
        self.deadline_nanos.store(deadline, Ordering::Release);
    }

    /// Removes an elapsed publication before the common IRQ path rearms the
    /// shared host comparator. The AxVM worker republishes the next wheel
    /// deadline after consuming all expired events.
    pub(crate) fn clear_if_elapsed(&self, now_nanos: u64) {
        let mut observed = self.deadline_nanos.load(Ordering::Acquire);
        while observed != NO_PUBLISHED_DEADLINE && observed <= now_nanos {
            match self.deadline_nanos.compare_exchange_weak(
                observed,
                NO_PUBLISHED_DEADLINE,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }
    }

    pub(crate) fn is_due(&self, now_nanos: u64) -> bool {
        self.deadline_nanos()
            .is_some_and(|deadline| deadline <= now_nanos)
    }
}

/// Owner-aware handle for one AxVM timer-wheel entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct VmTimerHandle {
    token: usize,
    owner_cpu: usize,
}

struct VmTimerEvent {
    token: usize,
    callback: Box<dyn FnOnce(TimeValue) + Send + 'static>,
}

impl VmTimerEvent {
    fn new<F>(token: usize, callback: F) -> Self
    where
        F: FnOnce(TimeValue) + Send + 'static,
    {
        Self {
            token,
            callback: Box::new(callback),
        }
    }
}

impl TimerEvent for VmTimerEvent {
    fn callback(self, now: TimeValue) {
        trace!("handle VM timer event token {}", self.token);
        (self.callback)(now);
    }
}

struct CpuTimerWheel {
    events: TimerList<VmTimerEvent>,
}

impl CpuTimerWheel {
    const fn new() -> Self {
        Self {
            events: TimerList::new(),
        }
    }

    fn register(&mut self, deadline: TimeValue, event: VmTimerEvent) -> Option<TimeValue> {
        self.events.set(deadline, event);
        self.next_deadline()
    }

    fn cancel(&mut self, token: usize) -> Option<Option<TimeValue>> {
        if self.events.cancel(|event| event.token == token) == 0 {
            return None;
        }
        Some(self.next_deadline())
    }

    fn expire_one(&mut self, now: TimeValue) -> Option<(TimeValue, VmTimerEvent)> {
        self.events.expire_one(now)
    }

    fn next_deadline(&self) -> Option<TimeValue> {
        self.events.next_deadline()
    }
}

#[cfg(not(feature = "global-timer-wheel"))]
static TIMER_WHEELS: [IrqSafeMutex<CpuTimerWheel>; MAX_TIMER_CPUS] =
    [const { IrqSafeMutex::new(CpuTimerWheel::new()) }; MAX_TIMER_CPUS];
#[cfg(feature = "global-timer-wheel")]
static TIMER_WHEELS: IrqSafeMutex<[CpuTimerWheel; MAX_TIMER_CPUS]> =
    IrqSafeMutex::new([const { CpuTimerWheel::new() }; MAX_TIMER_CPUS]);
static PUBLISHED_TIMER_DEADLINES: [PublishedTimerDeadline; MAX_TIMER_CPUS] =
    [const { PublishedTimerDeadline::new() }; MAX_TIMER_CPUS];

pub(crate) fn register_timer(
    deadline_ns: u64,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> usize {
    register_timer_handle(deadline_ns, callback).token
}

pub(crate) fn register_timer_handle(
    deadline_ns: u64,
    callback: Box<dyn FnOnce(Duration) + Send + 'static>,
) -> VmTimerHandle {
    let (handle, next_deadline) = with_current_timer_wheel(|cpu_id, timer_wheel| {
        let token = allocate_timer_token(cpu_id);
        let next_deadline = timer_wheel.register(
            TimeValue::from_nanos(deadline_ns),
            VmTimerEvent::new(token, callback),
        );
        published_timer_deadline(cpu_id).publish(next_deadline);
        (
            VmTimerHandle {
                token,
                owner_cpu: cpu_id,
            },
            next_deadline,
        )
    });
    if let Some(count) = TIMER_REGISTER_COUNTS.get(handle.owner_cpu) {
        count.fetch_add(1, Ordering::Relaxed);
    }
    rearm_host_timer(next_deadline);
    handle
}

pub(crate) fn cancel_timer_handle(handle: VmTimerHandle) {
    let _guard = PreemptGuard::new();
    let current_cpu = current_cpu_id();
    let next_deadline = with_cpu_timer_wheel(handle.owner_cpu, |timer_wheel| {
        let next_deadline = timer_wheel.cancel(handle.token);
        if let Some(next_deadline) = next_deadline {
            published_timer_deadline(handle.owner_cpu).publish(next_deadline);
        }
        next_deadline
    });
    if let Some(next_deadline) = next_deadline {
        if let Some(count) = TIMER_CANCEL_COUNTS.get(handle.owner_cpu) {
            count.fetch_add(1, Ordering::Relaxed);
        }
        rearm_owner_host_timer(handle.owner_cpu, current_cpu, next_deadline);
    }
}

pub(crate) fn cancel_timer(token: usize) {
    cancel_timer_handle(VmTimerHandle {
        token,
        owner_cpu: timer_token_owner(token),
    });
}

pub(crate) fn check_events() {
    check_events_bounded(usize::MAX);
}

fn check_events_bounded(event_budget: usize) {
    debug_assert!(event_budget > 0);
    let mut counted_batch = false;
    let mut processed = 0usize;
    loop {
        let now = current_host_time();
        let (expired, next_deadline) = with_current_timer_wheel(|cpu_id, timer_wheel| {
            let expired = timer_wheel.expire_one(now);
            published_timer_deadline(cpu_id).publish(timer_wheel.next_deadline());
            let next_deadline = if expired.is_none() {
                timer_wheel.next_deadline()
            } else {
                None
            };
            (expired, next_deadline)
        });
        if let Some((deadline, event)) = expired {
            let cpu_id = current_cpu_id();
            if let Some(count) = TIMER_EXPIRE_COUNTS.get(cpu_id) {
                count.fetch_add(1, Ordering::Relaxed);
            }
            let lateness_ns = now
                .as_nanos()
                .saturating_sub(deadline.as_nanos())
                .min(u64::MAX as u128) as u64;
            record_timer_expiry_lateness(cpu_id, lateness_ns);
            if !counted_batch {
                TIMER_EXPIRY_BATCH_COUNTS[cpu_id].fetch_add(1, Ordering::Relaxed);
                counted_batch = true;
            }
            trace!("handle VM timer event scheduled at {deadline:#?}");
            event.callback(now);
            processed = processed.saturating_add(1);
            if processed >= event_budget {
                let next_deadline =
                    with_current_timer_wheel(|_, timer_wheel| timer_wheel.next_deadline());
                rearm_host_timer(next_deadline);
                break;
            }
        } else {
            rearm_host_timer(next_deadline);
            break;
        }
    }
}

#[cfg(not(test))]
fn current_host_time() -> TimeValue {
    default_host().monotonic_time()
}

#[cfg(test)]
fn current_host_time() -> TimeValue {
    TimeValue::from_nanos(TEST_NOW_NS.load(Ordering::Acquire))
}

fn rearm_owner_host_timer(owner_cpu: usize, current_cpu: usize, next_deadline: Option<TimeValue>) {
    if owner_cpu == current_cpu {
        rearm_host_timer(next_deadline);
    } else {
        rearm_remote_owner_host_timer(owner_cpu);
    }
}

fn rearm_current_host_timer_from_wheel() {
    let next_deadline = with_current_timer_wheel(|_, timer_wheel| timer_wheel.next_deadline());
    rearm_host_timer(next_deadline);
}

#[cfg(not(test))]
unsafe fn rearm_current_host_timer_from_wheel_thunk(_arg: *mut ()) {
    rearm_current_host_timer_from_wheel();
}

#[cfg(not(test))]
fn rearm_remote_owner_host_timer(owner_cpu: usize) {
    let result = task::run_on_cpu_sync(
        owner_cpu,
        rearm_current_host_timer_from_wheel_thunk,
        std::ptr::null_mut(),
    );
    if let Err(error) = result {
        warn!("failed to rearm AxVM timer on owner CPU {owner_cpu}: {error:?}; sending IPI");
        task::send_ipi(owner_cpu);
    }
}

#[cfg(not(test))]
fn rearm_host_timer(next_deadline: Option<TimeValue>) {
    if let Some(deadline) = next_deadline {
        default_host().request_timer_deadline(deadline.as_nanos() as u64);
    }
}

pub(crate) fn init_percpu() {
    info!("Initializing AxVM timer wheel...");
    let cpu_id = current_cpu_id();
    let deadline_source = published_timer_deadline(cpu_id);
    let notify = Arc::new(IrqNotify::new());
    let worker_notify = notify.clone();
    let worker = crate::host::task::TaskInner::new(
        move || loop {
            worker_notify.wait();
            if let Some(count) = TIMER_WORKER_WAKE_COUNTS.get(cpu_id) {
                count.fetch_add(1, Ordering::Relaxed);
            }
            check_events_bounded(TIMER_WORKER_EVENT_BUDGET);
        },
        std::format!("axvm-timer-{cpu_id}"),
        TIMER_WORKER_STACK_SIZE,
    );
    worker.set_sched_priority(crate::runtime::TIMER_WORKER_TASK_PRIORITY);
    let cpu_bit = 1usize
        .checked_shl(cpu_id as u32)
        .expect("AxVM timer worker CPU ID must fit the host CPU mask");
    worker.set_cpumask(crate::host::task::cpu_mask_from_raw_bits(cpu_bit));
    crate::host::task::spawn_task(worker);
    crate::arch::register_timer_source(deadline_source, notify);
}

fn allocate_timer_token(owner_cpu: usize) -> usize {
    let sequence = TOKEN.fetch_add(1, Ordering::Relaxed);
    sequence
        .checked_mul(MAX_TIMER_CPUS)
        .and_then(|token| token.checked_add(owner_cpu))
        .expect("AxVM timer token space exhausted")
}

const fn timer_token_owner(token: usize) -> usize {
    token % MAX_TIMER_CPUS
}

fn published_timer_deadline(cpu_id: usize) -> &'static PublishedTimerDeadline {
    PUBLISHED_TIMER_DEADLINES
        .get(cpu_id)
        .expect("AxVM timer CPU ID must fit the host CPU mask")
}

fn with_cpu_timer_wheel<R>(cpu_id: usize, operation: impl FnOnce(&mut CpuTimerWheel) -> R) -> R {
    let wait_started = current_host_time().as_nanos() as u64;
    #[cfg(not(feature = "global-timer-wheel"))]
    let timer_wheel = TIMER_WHEELS
        .get(cpu_id)
        .expect("AxVM timer CPU ID must fit the host CPU mask");
    #[cfg(not(feature = "global-timer-wheel"))]
    let mut guard = timer_wheel.lock();
    #[cfg(feature = "global-timer-wheel")]
    let mut guard = TIMER_WHEELS.lock();
    let acquired = current_host_time().as_nanos() as u64;
    #[cfg(not(feature = "global-timer-wheel"))]
    let result = operation(&mut guard);
    #[cfg(feature = "global-timer-wheel")]
    let result = operation(
        guard
            .get_mut(cpu_id)
            .expect("AxVM timer CPU ID must fit the host CPU mask"),
    );
    let completed = current_host_time().as_nanos() as u64;
    record_lock_timing(
        cpu_id,
        acquired.saturating_sub(wait_started),
        completed.saturating_sub(acquired),
    );
    drop(guard);
    result
}

fn record_lock_timing(cpu_id: usize, wait_ns: u64, hold_ns: u64) {
    let Some(acquisitions) = TIMER_LOCK_ACQUISITIONS.get(cpu_id) else {
        return;
    };
    acquisitions.fetch_add(1, Ordering::Relaxed);
    TIMER_LOCK_WAIT_TOTAL_NS[cpu_id].fetch_add(wait_ns, Ordering::Relaxed);
    TIMER_LOCK_HOLD_TOTAL_NS[cpu_id].fetch_add(hold_ns, Ordering::Relaxed);
    TIMER_LOCK_WAIT_MAX_NS[cpu_id].fetch_max(wait_ns, Ordering::Relaxed);
    TIMER_LOCK_HOLD_MAX_NS[cpu_id].fetch_max(hold_ns, Ordering::Relaxed);
}

#[cfg(not(test))]
fn reset_lock_timing() {
    for cpu_id in 0..MAX_RT_STATS_CPUS {
        TIMER_LOCK_ACQUISITIONS[cpu_id].store(0, Ordering::Relaxed);
        TIMER_LOCK_WAIT_TOTAL_NS[cpu_id].store(0, Ordering::Relaxed);
        TIMER_LOCK_WAIT_MAX_NS[cpu_id].store(0, Ordering::Relaxed);
        TIMER_LOCK_HOLD_TOTAL_NS[cpu_id].store(0, Ordering::Relaxed);
        TIMER_LOCK_HOLD_MAX_NS[cpu_id].store(0, Ordering::Relaxed);
    }
}

#[cfg(not(test))]
pub(crate) fn run_timer_storm(
    cpu_mask: usize,
    iterations_per_worker: usize,
    expiry_samples_per_worker: usize,
    expiry_delay: Duration,
) -> Result<crate::TimerStormResult, &'static str> {
    use std::sync::atomic::AtomicBool;

    const STORM_STACK_SIZE: usize = 0x20_000;
    const STORM_CONTROL_PRIORITY: i32 = 99;
    const STORM_YIELD_BATCH: usize = 64;
    const FAR_FUTURE_NS: u64 = 3_600_000_000_000;
    const READY_TIMEOUT: Duration = Duration::from_secs(10);
    const EXPIRY_TIMEOUT: Duration = Duration::from_secs(30);

    if cpu_mask == 0 {
        return Err("CPU mask must select at least one CPU");
    }
    if iterations_per_worker == 0 {
        return Err("iterations per worker must be non-zero");
    }
    let available_mask = crate::percpu::enabled_cpu_mask()
        & ((1usize << MAX_RT_STATS_CPUS.min(usize::BITS as usize)) - 1);
    if cpu_mask & !available_mask != 0 {
        return Err("CPU mask includes an unavailable or untracked CPU");
    }

    let cpus: Vec<usize> = (0..MAX_RT_STATS_CPUS)
        .filter(|cpu_id| cpu_mask & (1usize << cpu_id) != 0)
        .collect();
    let worker_count = cpus.len();
    let ready = Arc::new(AtomicUsize::new(0));
    let start = Arc::new(AtomicBool::new(false));
    let mut workers = Vec::with_capacity(worker_count);
    let command_task = crate::host::task::current_task();
    let command_priority = command_task.sched_priority();
    command_task.set_sched_priority(STORM_CONTROL_PRIORITY);

    for cpu_id in cpus.iter().copied() {
        let worker_ready = ready.clone();
        let worker_start = start.clone();
        let worker = crate::host::task::TaskInner::new(
            move || {
                worker_ready.fetch_add(1, Ordering::Release);
                while !worker_start.load(Ordering::Acquire) {
                    crate::host::task::yield_now();
                }
                let started = current_host_time().as_nanos() as u64;
                let deadline = started.saturating_add(FAR_FUTURE_NS);
                for iteration in 0..iterations_per_worker {
                    let handle = register_timer_handle(deadline, Box::new(|_| {}));
                    cancel_timer_handle(handle);
                    if (iteration + 1) % STORM_YIELD_BATCH == 0 {
                        crate::host::task::yield_now();
                    }
                }
            },
            std::format!("timer-storm-{cpu_id}"),
            STORM_STACK_SIZE,
        );
        worker.set_sched_priority(crate::runtime::VCPU_TASK_PRIORITY);
        worker.set_cpumask(crate::host::task::cpu_mask_from_raw_bits(1usize << cpu_id));
        workers.push(crate::host::task::spawn_task(worker));
    }

    let ready_started = current_host_time();
    while ready.load(Ordering::Acquire) != worker_count {
        crate::host::task::yield_now();
        if current_host_time().saturating_sub(ready_started) >= READY_TIMEOUT {
            start.store(true, Ordering::Release);
            for worker in workers {
                worker.join();
            }
            command_task.set_sched_priority(command_priority);
            return Err("timer-storm workers did not reach the start barrier");
        }
    }
    reset_lock_timing();
    let wall_started = current_host_time().as_nanos() as u64;
    command_task.set_sched_priority(command_priority);
    start.store(true, Ordering::Release);
    for worker in workers {
        worker.join();
    }
    let elapsed_ns = (current_host_time().as_nanos() as u64).saturating_sub(wall_started);

    let expiry_samples = worker_count.saturating_mul(expiry_samples_per_worker);
    let expiry_completed = Arc::new(AtomicUsize::new(0));
    let expiry_lateness: Arc<Vec<AtomicU64>> =
        Arc::new((0..expiry_samples).map(|_| AtomicU64::new(0)).collect());
    let expiry_ready = Arc::new(AtomicUsize::new(0));
    let expiry_start = Arc::new(AtomicBool::new(false));
    let mut expiry_workers = Vec::with_capacity(worker_count);
    command_task.set_sched_priority(STORM_CONTROL_PRIORITY);

    for (worker_index, cpu_id) in cpus.iter().copied().enumerate() {
        let worker_ready = expiry_ready.clone();
        let worker_start = expiry_start.clone();
        let completed = expiry_completed.clone();
        let lateness = expiry_lateness.clone();
        let worker = crate::host::task::TaskInner::new(
            move || {
                worker_ready.fetch_add(1, Ordering::Release);
                while !worker_start.load(Ordering::Acquire) {
                    crate::host::task::yield_now();
                }
                let deadline = (current_host_time() + expiry_delay).as_nanos() as u64;
                for sample in 0..expiry_samples_per_worker {
                    let completed = completed.clone();
                    let lateness = lateness.clone();
                    let index = worker_index * expiry_samples_per_worker + sample;
                    register_timer_handle(
                        deadline,
                        Box::new(move |now| {
                            let late_ns = (now.as_nanos() as u64).saturating_sub(deadline);
                            lateness[index].store(late_ns.saturating_add(1), Ordering::Release);
                            completed.fetch_add(1, Ordering::Release);
                        }),
                    );
                }
            },
            std::format!("timer-expiry-storm-{cpu_id}"),
            STORM_STACK_SIZE,
        );
        worker.set_sched_priority(crate::runtime::VCPU_TASK_PRIORITY);
        worker.set_cpumask(crate::host::task::cpu_mask_from_raw_bits(1usize << cpu_id));
        expiry_workers.push(crate::host::task::spawn_task(worker));
    }

    let expiry_ready_started = current_host_time();
    while expiry_ready.load(Ordering::Acquire) != worker_count {
        crate::host::task::yield_now();
        if current_host_time().saturating_sub(expiry_ready_started) >= READY_TIMEOUT {
            expiry_start.store(true, Ordering::Release);
            for worker in expiry_workers {
                worker.join();
            }
            command_task.set_sched_priority(command_priority);
            return Err("timer expiry workers did not reach the start barrier");
        }
    }
    command_task.set_sched_priority(command_priority);
    expiry_start.store(true, Ordering::Release);
    for worker in expiry_workers {
        worker.join();
    }
    crate::host::task::yield_now();
    let expiry_wait_started = current_host_time();
    while expiry_completed.load(Ordering::Acquire) != expiry_samples {
        crate::host::task::yield_now();
        if current_host_time().saturating_sub(expiry_wait_started) >= EXPIRY_TIMEOUT {
            break;
        }
    }

    let completed = expiry_completed.load(Ordering::Acquire).min(expiry_samples);
    let mut observed_lateness: Vec<u64> = expiry_lateness
        .iter()
        .filter_map(|value| value.load(Ordering::Acquire).checked_sub(1))
        .collect();
    observed_lateness.sort_unstable();
    let lock_stats = rt_timer_stats_snapshot();
    let lock_acquisitions = lock_stats
        .iter()
        .map(|counts| counts.lock_acquisitions)
        .sum();
    let lock_wait_total_ns = lock_stats
        .iter()
        .map(|counts| counts.lock_wait_total_ns)
        .sum();
    let lock_wait_max_ns = lock_stats
        .iter()
        .map(|counts| counts.lock_wait_max_ns)
        .max()
        .unwrap_or(0);
    let lock_hold_total_ns = lock_stats
        .iter()
        .map(|counts| counts.lock_hold_total_ns)
        .sum();
    let lock_hold_max_ns = lock_stats
        .iter()
        .map(|counts| counts.lock_hold_max_ns)
        .max()
        .unwrap_or(0);
    let register_cancel_pairs = worker_count.saturating_mul(iterations_per_worker);
    let pairs_per_second = if elapsed_ns == 0 {
        0
    } else {
        ((register_cancel_pairs as u128 * 1_000_000_000u128) / elapsed_ns as u128)
            .min(u64::MAX as u128) as u64
    };

    Ok(crate::TimerStormResult {
        implementation: if cfg!(feature = "global-timer-wheel") {
            "global-lock"
        } else {
            "per-cpu-lock"
        },
        cpu_mask,
        workers: worker_count,
        iterations_per_worker,
        register_cancel_pairs,
        elapsed_ns,
        pairs_per_second,
        expiry_samples,
        expiry_completed: completed,
        expiry_p50_late_ns: percentile(&observed_lateness, 50, 100),
        expiry_p99_late_ns: percentile(&observed_lateness, 99, 100),
        expiry_max_late_ns: observed_lateness.last().copied().unwrap_or(0),
        lock_acquisitions,
        lock_wait_total_ns,
        lock_wait_max_ns,
        lock_hold_total_ns,
        lock_hold_max_ns,
    })
}

#[cfg(not(test))]
fn percentile(sorted: &[u64], numerator: usize, denominator: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = sorted
        .len()
        .saturating_mul(numerator)
        .saturating_add(denominator - 1)
        / denominator;
    sorted[rank.saturating_sub(1).min(sorted.len() - 1)]
}

fn with_current_timer_wheel<R>(operation: impl FnOnce(usize, &mut CpuTimerWheel) -> R) -> R {
    let _guard = PreemptGuard::new();
    let cpu_id = current_cpu_id();
    with_cpu_timer_wheel(cpu_id, |timer_wheel| operation(cpu_id, timer_wheel))
}

#[cfg(not(test))]
fn current_cpu_id() -> usize {
    use crate::host::HostCpu;

    default_host().this_cpu_id()
}

#[cfg(test)]
static TEST_CURRENT_CPU: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TEST_REARMS: Mutex<Vec<(usize, Option<TimeValue>)>> = Mutex::new(Vec::new());
#[cfg(test)]
static TEST_REMOTE_REARMS: Mutex<Vec<usize>> = Mutex::new(Vec::new());
#[cfg(test)]
static TEST_NOW_NS: AtomicU64 = AtomicU64::new(0);

#[cfg(test)]
fn current_cpu_id() -> usize {
    TEST_CURRENT_CPU.load(Ordering::Acquire)
}

#[cfg(test)]
fn lock_test_mutex<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().expect("AxVM timer test mutex poisoned")
}

#[cfg(test)]
fn rearm_host_timer(next_deadline: Option<TimeValue>) {
    lock_test_mutex(&TEST_REARMS).push((current_cpu_id(), next_deadline));
}

#[cfg(test)]
fn rearm_remote_owner_host_timer(owner_cpu: usize) {
    lock_test_mutex(&TEST_REMOTE_REARMS).push(owner_cpu);
    let previous_cpu = TEST_CURRENT_CPU.swap(owner_cpu, Ordering::AcqRel);
    rearm_current_host_timer_from_wheel();
    TEST_CURRENT_CPU.store(previous_cpu, Ordering::Release);
}

#[cfg(test)]
mod tests {
    use super::*;

    static TEST_LOCK: Mutex<()> = Mutex::new(());

    fn reset_global_timer_state() {
        for cpu_id in 0..MAX_TIMER_CPUS {
            with_cpu_timer_wheel(cpu_id, |timer_wheel| *timer_wheel = CpuTimerWheel::new());
            published_timer_deadline(cpu_id).publish(None);
        }
        lock_test_mutex(&TEST_REARMS).clear();
        lock_test_mutex(&TEST_REMOTE_REARMS).clear();
        TEST_CURRENT_CPU.store(0, Ordering::Release);
        TEST_NOW_NS.store(0, Ordering::Release);
        #[cfg(feature = "timer-latency-stats")]
        for cpu_id in 0..MAX_RT_STATS_CPUS {
            for count in &TIMER_EXPIRY_LATE_HISTOGRAMS[cpu_id] {
                count.store(0, Ordering::Relaxed);
            }
            TIMER_EXPIRY_LATE_OVERFLOWS[cpu_id].store(0, Ordering::Relaxed);
            TIMER_EXPIRY_LATE_MAX_NS[cpu_id].store(0, Ordering::Relaxed);
        }
    }

    fn set_current_cpu_for_test(cpu_id: usize) {
        TEST_CURRENT_CPU.store(cpu_id, Ordering::Release);
    }

    static TEST_CALLBACK_NOW_NS: AtomicU64 = AtomicU64::new(0);

    fn event(token: usize) -> VmTimerEvent {
        VmTimerEvent::new(token, |_| {})
    }

    #[test]
    fn host_timer_callback_path_dispatches_registered_event_once() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();
        TEST_CALLBACK_NOW_NS.store(0, Ordering::Release);

        set_current_cpu_for_test(0);
        TEST_NOW_NS.store(1_000_000, Ordering::Release);
        let token = register_timer(
            10_000_000,
            Box::new(|now| {
                TEST_CALLBACK_NOW_NS.store(now.as_nanos() as u64, Ordering::Release);
            }),
        );

        check_events();
        assert_eq!(TEST_CALLBACK_NOW_NS.load(Ordering::Acquire), 0);
        assert_eq!(
            lock_test_mutex(&TEST_REARMS).last().copied(),
            Some((0, Some(Duration::from_nanos(10_000_000))))
        );

        TEST_NOW_NS.store(10_000_000, Ordering::Release);
        check_events();
        assert_eq!(TEST_CALLBACK_NOW_NS.load(Ordering::Acquire), 10_000_000);
        assert_eq!(
            with_cpu_timer_wheel(timer_token_owner(token), |timer_wheel| {
                timer_wheel.cancel(token)
            }),
            None
        );
    }

    #[test]
    fn bounded_timer_dispatch_processes_at_most_one_callback_per_pass() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();
        set_current_cpu_for_test(0);
        TEST_NOW_NS.store(10_000, Ordering::Release);
        let completed = Arc::new(AtomicUsize::new(0));

        for _ in 0..2 {
            let completed = completed.clone();
            register_timer(
                10_000,
                Box::new(move |_| {
                    completed.fetch_add(1, Ordering::Release);
                }),
            );
        }

        check_events_bounded(1);
        assert_eq!(completed.load(Ordering::Acquire), 1);
        assert_eq!(
            lock_test_mutex(&TEST_REARMS).last().copied(),
            Some((0, Some(Duration::from_nanos(10_000))))
        );

        check_events_bounded(1);
        assert_eq!(completed.load(Ordering::Acquire), 2);
    }

    #[test]
    fn timer_snapshot_observes_registration_and_expiration() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();
        set_current_cpu_for_test(2);
        TEST_NOW_NS.store(1_000, Ordering::Release);
        let before = crate::rt_runtime_stats_snapshot();

        register_timer(2_000, Box::new(|_| {}));
        TEST_NOW_NS.store(2_000, Ordering::Release);
        check_events();

        let after = crate::rt_runtime_stats_snapshot();
        assert_eq!(after.timers[2].registered, before.timers[2].registered + 1);
        assert_eq!(after.timers[2].expired, before.timers[2].expired + 1);
    }

    #[cfg(feature = "timer-latency-stats")]
    #[test]
    fn timer_snapshot_reports_expiry_lateness_percentiles_and_overflow() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();
        set_current_cpu_for_test(2);

        for lateness_ns in [0, 500, 1_000, 3_999_000, 5_000_000] {
            record_timer_expiry_lateness(2, lateness_ns);
        }

        let counts = rt_timer_stats_snapshot()[2];
        assert_eq!(counts.expiry_late_samples, 5);
        assert_eq!(counts.expiry_late_overflow, 1);
        assert_eq!(counts.expiry_late_p50_ns, 2_000);
        assert_eq!(counts.expiry_late_p99_ns, 5_000_000);
        assert_eq!(counts.expiry_late_p99_9_ns, 5_000_000);
        assert_eq!(counts.expiry_late_max_ns, 5_000_000);
    }

    #[test]
    fn cancel_removes_event_from_original_cpu_wheel() {
        let mut timer_wheel = CpuTimerWheel::new();
        let deadline = Duration::from_secs(60);

        assert_eq!(timer_wheel.register(deadline, event(7)), Some(deadline));
        assert_eq!(timer_wheel.next_deadline(), Some(deadline));

        assert_eq!(timer_wheel.cancel(7), Some(None));
        assert_eq!(timer_wheel.next_deadline(), None);
        assert_eq!(timer_wheel.cancel(7), None);
    }

    #[test]
    fn cancel_rearms_to_remaining_owner_deadline() {
        let mut timer_wheel = CpuTimerWheel::new();
        let early = Duration::from_secs(10);
        let late = Duration::from_secs(20);

        timer_wheel.register(early, event(11));
        timer_wheel.register(late, event(12));

        assert_eq!(timer_wheel.cancel(11), Some(Some(late)));
        assert_eq!(timer_wheel.next_deadline(), Some(late));
    }

    #[test]
    fn migration_reprogramming_deletes_stale_original_cpu_deadline() {
        let mut original_wheel = CpuTimerWheel::new();
        let mut migrated_wheel = CpuTimerWheel::new();
        let stale_deadline = Duration::from_secs(60);
        let migrated_deadline = Duration::from_millis(10);

        assert_eq!(
            original_wheel.register(stale_deadline, event(31)),
            Some(stale_deadline)
        );
        assert_eq!(original_wheel.cancel(31), Some(None));
        assert_eq!(
            migrated_wheel.register(migrated_deadline, event(32)),
            Some(migrated_deadline)
        );

        assert!(original_wheel.expire_one(stale_deadline).is_none());
        let (deadline, migrated_event) = migrated_wheel
            .expire_one(migrated_deadline)
            .expect("migrated timer event should expire on the new owner CPU");
        assert_eq!(deadline, migrated_deadline);
        assert_eq!(migrated_event.token, 32);
        assert_eq!(migrated_wheel.cancel(32), None);
    }

    #[test]
    fn expired_event_cannot_be_cancelled_again() {
        let mut timer_wheel = CpuTimerWheel::new();
        let deadline = Duration::from_millis(5);

        timer_wheel.register(deadline, event(21));
        let expired = timer_wheel.expire_one(deadline);

        assert!(expired.is_some());
        assert_eq!(timer_wheel.cancel(21), None);
    }

    #[test]
    fn published_deadline_tracks_registration_cancellation_and_expiry() {
        let mut timer_wheel = CpuTimerWheel::new();
        let source = PublishedTimerDeadline::new();
        let early = Duration::from_millis(5);
        let late = Duration::from_millis(10);

        source.publish(timer_wheel.register(early, event(51)));
        source.publish(timer_wheel.register(late, event(52)));
        assert_eq!(source.deadline_nanos(), Some(5_000_000));

        source.publish(timer_wheel.cancel(51).expect("timer must exist"));
        assert_eq!(source.deadline_nanos(), Some(10_000_000));

        timer_wheel.expire_one(late);
        source.publish(timer_wheel.next_deadline());
        assert_eq!(source.deadline_nanos(), None);
    }

    #[test]
    fn timer_irq_clears_only_an_elapsed_publication() {
        let source = PublishedTimerDeadline::new();
        source.publish(Some(Duration::from_nanos(20)));

        source.clear_if_elapsed(19);
        assert_eq!(source.deadline_nanos(), Some(20));

        source.clear_if_elapsed(25);
        assert_eq!(source.deadline_nanos(), None);
    }

    #[test]
    fn published_deadline_reports_due_state_without_consuming_it() {
        let source = PublishedTimerDeadline::new();
        source.publish(Some(Duration::from_nanos(20)));

        assert!(!source.is_due(19));
        assert!(source.is_due(20));
        assert!(source.is_due(25));
        assert_eq!(source.deadline_nanos(), Some(20));
    }

    #[test]
    fn remote_cancel_reprograms_owner_cpu_timer() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();

        set_current_cpu_for_test(0);
        let early_token = register_timer(10_000_000, Box::new(|_| {}));
        let late_token = register_timer(20_000_000, Box::new(|_| {}));
        assert_eq!(lock_test_mutex(&TEST_REARMS).len(), 2);

        lock_test_mutex(&TEST_REARMS).clear();
        set_current_cpu_for_test(1);
        cancel_timer(early_token);

        assert_eq!(lock_test_mutex(&TEST_REMOTE_REARMS).as_slice(), &[0]);
        assert_eq!(
            lock_test_mutex(&TEST_REARMS).as_slice(),
            &[(0, Some(Duration::from_nanos(20_000_000)))]
        );

        lock_test_mutex(&TEST_REARMS).clear();
        cancel_timer(late_token);

        assert_eq!(lock_test_mutex(&TEST_REMOTE_REARMS).as_slice(), &[0, 0]);
        assert_eq!(lock_test_mutex(&TEST_REARMS).as_slice(), &[(0, None)]);
    }

    #[test]
    fn owner_aware_handle_rejects_a_stale_cpu_identity() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();
        let deadline = Duration::from_secs(1);
        set_current_cpu_for_test(2);
        let handle = register_timer_handle(deadline.as_nanos() as u64, Box::new(|_| {}));

        cancel_timer_handle(VmTimerHandle {
            token: handle.token,
            owner_cpu: 1,
        });
        assert_eq!(
            with_cpu_timer_wheel(2, |timer_wheel| timer_wheel.next_deadline()),
            Some(deadline)
        );

        cancel_timer_handle(handle);
        assert_eq!(
            with_cpu_timer_wheel(2, |timer_wheel| timer_wheel.next_deadline()),
            None
        );
    }

    #[test]
    fn timer_token_round_trips_owner_cpu() {
        for owner_cpu in 0..MAX_TIMER_CPUS {
            assert_eq!(
                timer_token_owner(allocate_timer_token(owner_cpu)),
                owner_cpu
            );
        }
    }

    #[cfg(not(feature = "global-timer-wheel"))]
    #[test]
    fn one_cpu_wheel_lock_does_not_block_another_cpu() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();
        let (locked_tx, locked_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let cpu0 = std::thread::spawn(move || {
            with_cpu_timer_wheel(0, |_| {
                locked_tx.send(()).unwrap();
                release_rx.recv().unwrap();
            });
        });
        locked_rx.recv().unwrap();

        let (cpu1_done_tx, cpu1_done_rx) = std::sync::mpsc::channel();
        let cpu1 = std::thread::spawn(move || {
            with_cpu_timer_wheel(1, |timer_wheel| {
                timer_wheel.register(Duration::from_millis(1), event(99));
            });
            cpu1_done_tx.send(()).unwrap();
        });

        cpu1_done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("CPU1 timer wheel must not wait for the CPU0 wheel lock");
        release_tx.send(()).unwrap();
        cpu0.join().unwrap();
        cpu1.join().unwrap();
    }

    #[test]
    fn remote_handle_cancel_reprograms_the_recorded_owner_cpu() {
        let _guard = lock_test_mutex(&TEST_LOCK);
        reset_global_timer_state();

        set_current_cpu_for_test(2);
        let handle = register_timer_handle(20_000_000, Box::new(|_| {}));
        lock_test_mutex(&TEST_REARMS).clear();

        set_current_cpu_for_test(0);
        cancel_timer_handle(handle);

        assert_eq!(lock_test_mutex(&TEST_REMOTE_REARMS).as_slice(), &[2]);
        assert_eq!(lock_test_mutex(&TEST_REARMS).as_slice(), &[(2, None)]);
    }
}
