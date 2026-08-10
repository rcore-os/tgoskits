//! Rhealstone-style microbenchmarks for the isolated RT executor.
//!
//! This module adapts the FreeRTOS guest benchmark shape used by
//! `buhenxihuan/freertos-guest`: task switch, preemption wakeup, periodic
//! deadline latency, periodic jitter, and semaphore shuffle. The measurements
//! are intentionally RT-local; the host core only prints the final statistics so
//! benchmark tasks do not contend on the host console lock.

use core::sync::atomic::{AtomicU64, Ordering};

use log::{info, warn};

use crate::{
    RtSemaphore, RtTask, rt_delay_until, rt_exit_current_task, rt_monotonic_nanos, rt_sleep,
    rt_yield_now,
};

const SWITCH_ITERATIONS: u64 = 1_000;
const WAKEUP_ITERATIONS: u64 = 1_000;
const TICK_ITERATIONS: u64 = 500;
const TICK_PERIOD_NANOS: u64 = 1_000_000;
const BENCHMARK_TIMEOUT_NANOS: u64 = 8_000_000_000;

static TASK_SWITCH_STATS: BenchmarkStats = BenchmarkStats::new();
static PREEMPTION_STATS: BenchmarkStats = BenchmarkStats::new();
static IRQ_LATENCY_STATS: BenchmarkStats = BenchmarkStats::new();
static TICK_DELTA_STATS: BenchmarkStats = BenchmarkStats::new();
static SEMAPHORE_SHUFFLE_STATS: BenchmarkStats = BenchmarkStats::new();

static TASK_SWITCH_WAKE_A: RtSemaphore = RtSemaphore::new(0);
static TASK_SWITCH_WAKE_B: RtSemaphore = RtSemaphore::new(0);
static TASK_SWITCH_TIMESTAMP: AtomicU64 = AtomicU64::new(0);
static TASK_SWITCH_DONE: AtomicU64 = AtomicU64::new(0);

static PREEMPTION_WAKE_HIGH: RtSemaphore = RtSemaphore::new(0);
static PREEMPTION_TIMESTAMP: AtomicU64 = AtomicU64::new(0);
static PREEMPTION_DONE: AtomicU64 = AtomicU64::new(0);

static TICK_DONE: AtomicU64 = AtomicU64::new(0);

static SEMAPHORE_SHUFFLE_WAKE_HIGH: RtSemaphore = RtSemaphore::new(0);
static SEMAPHORE_SHUFFLE_TIMESTAMP: AtomicU64 = AtomicU64::new(0);
static SEMAPHORE_SHUFFLE_DONE: AtomicU64 = AtomicU64::new(0);

/// RT-side tasks that make up the benchmark suite.
pub const BENCHMARK_TASKS: [RtTask; 7] = [
    RtTask::with_priority("bench-switch-a", 0, 21, task_switch_a),
    RtTask::with_priority("bench-switch-b", 0, 21, task_switch_b),
    RtTask::with_priority("bench-pre-high", 0, 31, preemption_high),
    RtTask::with_priority("bench-pre-low", 0, 11, preemption_low),
    RtTask::with_priority("bench-tick", TICK_PERIOD_NANOS, 22, tick_delta_task),
    RtTask::with_priority("bench-sem-high", 0, 32, semaphore_shuffle_high),
    RtTask::with_priority("bench-sem-low", 0, 12, semaphore_shuffle_low),
];

fn task_switch_a() -> ! {
    rt_sleep(20_000_000);
    for _ in 0..SWITCH_ITERATIONS {
        TASK_SWITCH_TIMESTAMP.store(rt_monotonic_nanos(), Ordering::Release);
        TASK_SWITCH_WAKE_B.release();
        TASK_SWITCH_WAKE_A.acquire();
    }
    rt_exit_current_task();
}

fn task_switch_b() -> ! {
    for _ in 0..SWITCH_ITERATIONS {
        TASK_SWITCH_WAKE_B.acquire();
        TASK_SWITCH_STATS
            .record(rt_monotonic_nanos() - TASK_SWITCH_TIMESTAMP.load(Ordering::Acquire));
        TASK_SWITCH_WAKE_A.release();
    }
    TASK_SWITCH_DONE.store(1, Ordering::Release);
    rt_exit_current_task();
}

fn preemption_high() -> ! {
    for _ in 0..WAKEUP_ITERATIONS {
        PREEMPTION_WAKE_HIGH.acquire();
        PREEMPTION_STATS
            .record(rt_monotonic_nanos() - PREEMPTION_TIMESTAMP.load(Ordering::Acquire));
    }
    PREEMPTION_DONE.store(1, Ordering::Release);
    rt_exit_current_task();
}

fn preemption_low() -> ! {
    rt_sleep(25_000_000);
    for _ in 0..WAKEUP_ITERATIONS {
        PREEMPTION_TIMESTAMP.store(rt_monotonic_nanos(), Ordering::Release);
        PREEMPTION_WAKE_HIGH.release();
        rt_yield_now();
    }
    rt_exit_current_task();
}

/*
    这里的 IRQ Latency 在 ax-rt 里是 RT executor 的 deadline wakeup latency，
    不是 FreeRTOS guest 里的硬件 timer IRQ 注入延迟；
    但它是当前实时 executor 语义下对 timer_deadline -> handler/task resumes 的等价可测实现。
*/
fn tick_delta_task() -> ! {
    rt_sleep(30_000_000);
    let mut next_deadline = rt_monotonic_nanos().saturating_add(TICK_PERIOD_NANOS);
    let mut last_start = 0;
    for _ in 0..TICK_ITERATIONS {
        rt_delay_until(next_deadline);
        let now = rt_monotonic_nanos();
        IRQ_LATENCY_STATS.record(now.saturating_sub(next_deadline));
        if last_start != 0 {
            TICK_DELTA_STATS.record(now - last_start);
        }
        last_start = now;
        next_deadline = next_deadline.saturating_add(TICK_PERIOD_NANOS);
    }
    TICK_DONE.store(1, Ordering::Release);
    rt_exit_current_task();
}

fn semaphore_shuffle_high() -> ! {
    for _ in 0..WAKEUP_ITERATIONS {
        SEMAPHORE_SHUFFLE_WAKE_HIGH.acquire();
        SEMAPHORE_SHUFFLE_STATS
            .record(rt_monotonic_nanos() - SEMAPHORE_SHUFFLE_TIMESTAMP.load(Ordering::Acquire));
    }
    SEMAPHORE_SHUFFLE_DONE.store(1, Ordering::Release);
    rt_exit_current_task();
}

fn semaphore_shuffle_low() -> ! {
    rt_sleep(35_000_000);
    for _ in 0..WAKEUP_ITERATIONS {
        SEMAPHORE_SHUFFLE_TIMESTAMP.store(rt_monotonic_nanos(), Ordering::Release);
        SEMAPHORE_SHUFFLE_WAKE_HIGH.release();
        rt_yield_now();
    }
    rt_exit_current_task();
}

/// Host-side benchmark driver configuration.
pub struct BenchmarkConfig {
    /// Monotonic time source, in nanoseconds.
    pub time_fn: fn() -> u64,
}

/// Waits for the RT benchmark tasks and logs their summary statistics.
pub fn run_host_benchmarks(config: &BenchmarkConfig) {
    let now = config.time_fn;
    let deadline = now().saturating_add(BENCHMARK_TIMEOUT_NANOS);
    while now() < deadline {
        if benchmarks_finished() {
            print_benchmark_report();
            return;
        }
        core::hint::spin_loop();
    }

    warn!(
        "[RT benchmark] timed out: switch={}, preemption={}, tick={}, semaphore={}",
        TASK_SWITCH_DONE.load(Ordering::Acquire),
        PREEMPTION_DONE.load(Ordering::Acquire),
        TICK_DONE.load(Ordering::Acquire),
        SEMAPHORE_SHUFFLE_DONE.load(Ordering::Acquire)
    );
}

fn benchmarks_finished() -> bool {
    TASK_SWITCH_DONE.load(Ordering::Acquire) == 1
        && PREEMPTION_DONE.load(Ordering::Acquire) == 1
        && TICK_DONE.load(Ordering::Acquire) == 1
        && SEMAPHORE_SHUFFLE_DONE.load(Ordering::Acquire) == 1
}

fn print_benchmark_report() {
    info!("===== RT Rhealstone Benchmark =====");
    log_stats("Task Switch", TASK_SWITCH_STATS.snapshot());
    log_stats("Preemption", PREEMPTION_STATS.snapshot());
    log_stats("IRQ Latency", IRQ_LATENCY_STATS.snapshot());
    log_stats("Tick Delta", TICK_DELTA_STATS.snapshot());
    info!("  [Tick Delta] expected={TICK_PERIOD_NANOS} ns");
    log_stats("Sem Shuffle", SEMAPHORE_SHUFFLE_STATS.snapshot());
    info!("AX_RT_RHEALSTONE_BENCHMARK_PASSED");
}

fn log_stats(name: &str, stats: BenchmarkStatsSnapshot) {
    if stats.count == 0 {
        warn!("  [{name}] no data");
        return;
    }
    info!(
        "  [{name}] n={} avg={} min={} max={} jitter={} ns",
        stats.count,
        stats.sum / stats.count,
        stats.min,
        stats.max,
        stats.max - stats.min
    );
}

struct BenchmarkStats {
    min: AtomicU64,
    max: AtomicU64,
    sum: AtomicU64,
    count: AtomicU64,
}

impl BenchmarkStats {
    const fn new() -> Self {
        Self {
            min: AtomicU64::new(u64::MAX),
            max: AtomicU64::new(0),
            sum: AtomicU64::new(0),
            count: AtomicU64::new(0),
        }
    }

    fn record(&self, value: u64) {
        self.update_min(value);
        self.update_max(value);
        self.sum.fetch_add(value, Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Release);
    }

    fn snapshot(&self) -> BenchmarkStatsSnapshot {
        BenchmarkStatsSnapshot {
            min: self.min.load(Ordering::Acquire),
            max: self.max.load(Ordering::Acquire),
            sum: self.sum.load(Ordering::Acquire),
            count: self.count.load(Ordering::Acquire),
        }
    }

    fn update_min(&self, value: u64) {
        let mut current = self.min.load(Ordering::Acquire);
        while value < current {
            match self
                .min
                .compare_exchange(current, value, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(updated) => current = updated,
            }
        }
    }

    fn update_max(&self, value: u64) {
        let mut current = self.max.load(Ordering::Acquire);
        while value > current {
            match self
                .max
                .compare_exchange(current, value, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => return,
                Err(updated) => current = updated,
            }
        }
    }
}

struct BenchmarkStatsSnapshot {
    min: u64,
    max: u64,
    sum: u64,
    count: u64,
}
