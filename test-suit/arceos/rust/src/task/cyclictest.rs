//! Periodic task wake-up latency measurement (cyclictest style).
//!
//! Each thread wakes on a fixed period and records the deviation between the
//! expected wake-up instant and the actual one. The deviation is the scheduler
//! latency under the active scheduling policy (this feature selects the RT
//! scheduler via `ax-std/sched-rt`). All threads run at the default priority
//! so the numbers reflect pure scheduling jitter without priority-competition
//! effects.

use std::{
    println,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
    vec::Vec,
};

/// Number of periodic measurement threads.
const THREADS: usize = 4;
/// Target period of each measurement thread.
const INTERVAL: Duration = Duration::from_millis(10);
/// Sampling loops per thread.
const LOOPS: usize = 200;
/// Upper bound for the reported max wake-up latency, in microseconds.
///
/// Intentionally loose: the point of this test is to *report* jitter numbers;
/// the assertion only guards against a completely broken scheduler.
const MAX_ACCEPTABLE_LATENCY_US: u64 = 50_000;

static MAX_LATENCY_US: AtomicU64 = AtomicU64::new(0);
static MIN_LATENCY_US: AtomicU64 = AtomicU64::new(u64::MAX);
static TOTAL_LATENCY_US: AtomicU64 = AtomicU64::new(0);
static LATE_WAKES: AtomicU64 = AtomicU64::new(0);

pub fn run() -> crate::TestResult {
    MAX_LATENCY_US.store(0, Ordering::Relaxed);
    MIN_LATENCY_US.store(u64::MAX, Ordering::Relaxed);
    TOTAL_LATENCY_US.store(0, Ordering::Relaxed);
    LATE_WAKES.store(0, Ordering::Relaxed);

    let handles: Vec<_> = (0..THREADS)
        .map(|_| {
            thread::spawn(|| {
                for _ in 0..LOOPS {
                    // Expected wake-up instant computed before sleeping, so a
                    // late wake-up shows up as positive latency (saturating).
                    let deadline = Instant::now() + INTERVAL;
                    thread::sleep(INTERVAL);
                    let latency = Instant::now() - deadline;
                    let us = latency.as_micros() as u64;
                    MAX_LATENCY_US.fetch_max(us, Ordering::Relaxed);
                    MIN_LATENCY_US.fetch_min(us, Ordering::Relaxed);
                    TOTAL_LATENCY_US.fetch_add(us, Ordering::Relaxed);
                    if us > 0 {
                        LATE_WAKES.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();

    for handle in handles {
        handle.join().expect("cyclictest thread panicked");
    }

    let samples = THREADS * LOOPS;
    let max = MAX_LATENCY_US.load(Ordering::Relaxed);
    let min = MIN_LATENCY_US.load(Ordering::Relaxed);
    let min = if min == u64::MAX { 0 } else { min };
    let avg = TOTAL_LATENCY_US.load(Ordering::Relaxed) / samples as u64;
    let late = LATE_WAKES.load(Ordering::Relaxed);
    println!(
        "cyclictest: threads={} interval_ms={} samples={} max_latency_us={} avg_latency_us={} \
         min_latency_us={} late_wakes={}",
        THREADS,
        INTERVAL.as_millis(),
        samples,
        max,
        avg,
        min,
        late,
    );

    if max > MAX_ACCEPTABLE_LATENCY_US {
        return Err("cyclictest wake-up latency exceeded bound");
    }
    Ok(())
}
