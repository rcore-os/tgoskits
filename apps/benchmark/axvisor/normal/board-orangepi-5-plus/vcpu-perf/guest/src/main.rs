//! Fixed CPU work under periodic guest wakeups and a host competitor.
use std::{
    hint::black_box,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

#[cfg(feature = "arceos")]
use ax_std as _;

const WINDOWS: usize = 5;
const WINDOW: Duration = Duration::from_secs(3);

// Generated from the suite's single baseline configuration.
include!(concat!(env!("OUT_DIR"), "/baseline.rs"));

#[inline(never)]
fn work(mut value: u64) -> u64 {
    for _ in 0..256 {
        value ^= value << 13;
        value ^= value >> 7;
        value ^= value << 17;
        value = black_box(value);
    }
    value
}

fn measure() -> Result<(), &'static str> {
    assert_eq!(
        work(1),
        7_868_507_841_278_345_766,
        "workload known-answer check"
    );
    let stop = Arc::new(AtomicBool::new(false));
    let wakes = Arc::new(AtomicU64::new(0));
    let ready = Arc::new(Barrier::new(2));
    let timer_stop = stop.clone();
    let timer_wakes = wakes.clone();
    let timer_ready = ready.clone();
    let timer = std::thread::spawn(move || {
        timer_ready.wait();
        while !timer_stop.load(Ordering::Acquire) {
            std::thread::sleep(Duration::from_millis(1));
            timer_wakes.fetch_add(1, Ordering::Relaxed);
        }
    });
    ready.wait();
    let mut checksum = 1u64;
    let mut samples = [(0u64, 0u128, 0u64, 0u64); WINDOWS];
    for index in 0..=WINDOWS {
        let before_wakes = wakes.load(Ordering::Relaxed);
        let start = Instant::now();
        let mut blocks = 0u64;
        while start.elapsed() < WINDOW {
            checksum = work(checksum);
            blocks += 1;
        }
        let elapsed_ns = start.elapsed().as_nanos();
        let timer_wakes = wakes.load(Ordering::Relaxed) - before_wakes;
        println!(
            "VCPU_PERF_SAMPLE index={index} blocks={blocks} elapsed_ns={elapsed_ns} \
             timer_wakes={timer_wakes} checksum={checksum}"
        );
        if index > 0 {
            samples[index - 1] = (blocks, elapsed_ns, timer_wakes, checksum);
        }
    }
    stop.store(true, Ordering::Release);
    timer.join().expect("timer worker must exit");
    println!("VCPU_PERF_DONE windows={WINDOWS}");
    let mut scores = [0.0f64; WINDOWS];
    for (score, (blocks, ns, wakes, checksum)) in scores.iter_mut().zip(samples) {
        if blocks == 0 || checksum == 0 || !(3_000_000_000..=6_000_000_000).contains(&ns) {
            return Err("invalid work or measurement duration");
        }
        let seconds = ns as f64 / 1e9;
        if (wakes as f64 / seconds) < MIN_WAKE_RATE {
            return Err("timer load missing or stalled");
        }
        *score = blocks as f64 / seconds;
    }
    scores.sort_by(f64::total_cmp);
    let score = scores[WINDOWS / 2];
    let threshold = BASELINE * (1.0 - MAX_REGRESSION_PERCENT / 100.0);
    println!(
        "VCPU_PERF_RESULT blocks_per_second={score:.2} baseline={BASELINE:.2} \
         threshold={threshold:.2} samples={scores:?}"
    );
    if score < threshold {
        return Err("throughput regressed beyond configured budget");
    }
    Ok(())
}

fn main() {
    match measure() {
        Ok(()) => println!("VCPU_PERF_PASS"),
        Err(reason) => println!("VCPU_PERF_FAIL {reason}"),
    }
    // Keep the VM alive until the runner has consumed the completion line.
    // Otherwise guest power-off can retire the virtual UART before it drains.
    loop {
        std::thread::park();
    }
}
