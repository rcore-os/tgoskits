//! Periodic task wake-up latency measurement (cyclictest style).
//!
//! Standalone ArceOS port of `test-suit/arceos/rust/src/task/cyclictest.rs`
//! so the same measurement can run both bare-metal and inside an Axvisor
//! guest, and the jitter numbers can be compared to quantify the
//! virtualization-added scheduling latency.
//!
//! QEMU TCG emulation is slow (several times slower for a guest), so the
//! parameters are smaller than the test-suite version: a single thread and
//! 100 samples per run. The output line keeps the test-suite format so
//! bare-metal and guest runs can be diffed directly.

use std::{
    println,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

use ax_std as _;

/// Number of periodic measurement threads.
const THREADS: usize = 1;
/// Target period of each measurement thread.
const INTERVAL: Duration = Duration::from_millis(10);
/// Sampling loops per thread.
const LOOPS: usize = 100;

static MAX_LATENCY_US: AtomicU64 = AtomicU64::new(0);
static MIN_LATENCY_US: AtomicU64 = AtomicU64::new(u64::MAX);
static TOTAL_LATENCY_US: AtomicU64 = AtomicU64::new(0);
static LATE_WAKES: AtomicU64 = AtomicU64::new(0);

fn main() {
    MAX_LATENCY_US.store(0, Ordering::Relaxed);
    MIN_LATENCY_US.store(u64::MAX, Ordering::Relaxed);
    TOTAL_LATENCY_US.store(0, Ordering::Relaxed);
    LATE_WAKES.store(0, Ordering::Relaxed);

    let handle = thread::spawn(|| {
        for _ in 0..LOOPS {
            // Expected wake-up instant computed before sleeping, so a late
            // wake-up shows up as positive latency (saturating).
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
    });
    handle.join().expect("cyclictest thread panicked");

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
}
