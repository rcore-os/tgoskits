//! Fixed CPU work under periodic guest wakeups and a bounded host task load.
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

fn main() {
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
    }
    stop.store(true, Ordering::Release);
    timer.join().expect("timer worker must exit");
    println!("VCPU_PERF_DONE windows={WINDOWS}");
    // Keep the VM alive until the runner has consumed the completion line.
    // Otherwise guest power-off can retire the virtual UART before it drains.
    loop {
        std::thread::park();
    }
}
