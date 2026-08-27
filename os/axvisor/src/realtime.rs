//! Periodic host realtime workload used by the AMP comparison test.

use alloc::{string::String, vec::Vec};
use core::time::Duration;

use ax_std::time::Instant;

const PERIOD: Duration = Duration::from_millis(1);
const WARMUP_SAMPLES: usize = 100;
const MEASURED_SAMPLES: usize = 1_000;
const REALTIME_PRIORITY: isize = 80;
const START_DELAY: Duration = Duration::from_secs(3);

/// Starts the host realtime latency workload on the configured realtime CPU.
pub fn start() {
    let cpu_id = ax_task::realtime_cpu_id()
        .expect("the realtime-benchmark feature requires a nonnegative REALTIME_CPU_ID");
    ax_task::spawn_realtime(
        run,
        String::from("amp-realtime-latency"),
        ax_task::default_task_stack_size(),
        REALTIME_PRIORITY,
    )
    .expect("failed to create host realtime latency task");
    info!("AMP_RT_START source=host cpu={cpu_id} period_us=1000");
}

fn run() {
    // Keep platform startup effects outside the measurement window.
    ax_hal::time::busy_wait_until(ax_hal::time::monotonic_time() + START_DELAY);
    info!("AMP_RT_READY source=host");
    let mut lateness_us = Vec::with_capacity(MEASURED_SAMPLES);
    let mut expected = Instant::now();

    for sample in 0..(WARMUP_SAMPLES + MEASURED_SAMPLES) {
        expected += PERIOD;
        let now = Instant::now();
        let wait = expected.duration_since(now);
        if !wait.is_zero() {
            // Keep the first comparison independent of the shared axtask timer
            // wheel. The realtime CPU is reserved precisely to avoid timer
            // wakeups migrating through ordinary run queues.
            ax_hal::time::busy_wait_until(ax_hal::time::monotonic_time() + wait);
        }
        let lateness = Instant::now().duration_since(expected).as_micros() as u64;
        if sample >= WARMUP_SAMPLES {
            lateness_us.push(lateness);
        }
    }

    lateness_us.sort_unstable();
    let max = *lateness_us.last().unwrap_or(&0);
    let p50 = percentile(&lateness_us, 50);
    let p99 = percentile(&lateness_us, 99);
    let missed = lateness_us
        .iter()
        .filter(|latency| **latency >= 1_000)
        .count();
    info!(
        "AMP_RT_RESULT source=host samples={} period_us=1000 p50_us={} p99_us={} max_us={} missed={}",
        lateness_us.len(),
        p50,
        p99,
        max,
        missed
    );

    // A realtime CPU is intentionally outside the normal scheduler domain.
    // Keep its owner task resident after the finite benchmark instead of
    // entering the ordinary task-exit/reschedule path on that CPU.
    loop {
        core::hint::spin_loop();
    }
}

fn percentile(sorted: &[u64], percentile: usize) -> u64 {
    let index = sorted.len().saturating_sub(1) * percentile / 100;
    sorted.get(index).copied().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::percentile;

    #[test]
    fn percentile_uses_sorted_nearest_rank_index() {
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 50), 3);
        assert_eq!(percentile(&[1, 2, 3, 4, 5], 99), 4);
        assert_eq!(percentile(&[], 99), 0);
    }
}
