//! Periodic wake-up jitter benchmark for AxVisor Task 1 baseline collection.
//!
//! Output lines use the `RT_LATENCY` prefix so scripts and CI can parse results.

use std::{
    println, thread,
    time::{Duration, Instant},
    vec::Vec,
};

#[cfg(feature = "rt-latency-long")]
const DEFAULT_SAMPLES: usize = 180_000;
#[cfg(all(not(feature = "rt-latency-long"), feature = "rt-latency-stress-short"))]
const DEFAULT_SAMPLES: usize = 18_000;
#[cfg(all(
    not(feature = "rt-latency-long"),
    not(feature = "rt-latency-stress-short")
))]
const DEFAULT_SAMPLES: usize = 200;

const PERIODS_MS: &[u64] = &[1, 10];

#[cfg(feature = "rt-latency-guest")]
const MODE: &str = "guest";
#[cfg(all(feature = "rt-latency", not(feature = "rt-latency-guest")))]
const MODE: &str = "bare";

pub fn run() -> crate::TestResult {
    for &period_ms in PERIODS_MS {
        measure_period(period_ms)?;
    }
    println!("RT_LATENCY_PASS");
    Ok(())
}

fn measure_period(period_ms: u64) -> crate::TestResult {
    let period = Duration::from_millis(period_ms);
    let samples = DEFAULT_SAMPLES;
    let mut jitters = Vec::with_capacity(samples);

    for _ in 0..samples {
        let start = Instant::now();
        thread::sleep(period);
        let elapsed = start.elapsed();
        let jitter = elapsed.saturating_sub(period).as_nanos() as u64;
        jitters.push(jitter);
    }

    jitters.sort_unstable();
    let sum: u128 = jitters.iter().map(|v| *v as u128).sum();
    let mean_jitter_ns = (sum / samples as u128) as u64;
    let p99_jitter_ns = percentile(&jitters, 99, 100);
    let p999_jitter_ns = percentile(&jitters, 999, 1000);
    let max_jitter_ns = *jitters.last().unwrap_or(&0);

    println!(
        "RT_LATENCY mode={MODE} period_ms={period_ms} samples={samples} \
         mean_jitter_ns={mean_jitter_ns} p99_jitter_ns={p99_jitter_ns} \
         p999_jitter_ns={p999_jitter_ns} max_jitter_ns={max_jitter_ns}"
    );
    Ok(())
}

fn percentile(sorted: &[u64], rank: usize, out_of: usize) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let index = (sorted.len() - 1) * rank / out_of;
    sorted[index]
}
