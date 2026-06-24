#[cfg(not(feature = "arceos"))]
use std::thread;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Instant,
};

use arceos_thread_perf::{BenchStats, estimated_switch_ns};
#[cfg(feature = "arceos")]
use ax_std::{self as _, thread};

const CREATE_ITERS: u64 = 100_000;
const SWITCH_ITERS: u64 = 1_000_000;
const WARMUP_ITERS: u64 = 1_000;
const THREAD_STACK_SIZE: usize = 64 * 1024;

fn main() {
    #[cfg(feature = "arceos")]
    println!("=== ArceOS thread performance benchmark ===\n");
    #[cfg(not(feature = "arceos"))]
    println!("=== Linux Rust thread performance benchmark ===\n");
    println!("fixed create/join iters: {CREATE_ITERS}");
    println!("fixed switch iters: {SWITCH_ITERS}");
    println!("fixed warmup iters: {WARMUP_ITERS}");
    println!("thread stack size: {THREAD_STACK_SIZE} bytes");
    #[cfg(feature = "arceos")]
    println!("note: uses ax_std::thread for spawn/join/yield_now.\n");
    #[cfg(not(feature = "arceos"))]
    println!("note: uses std::thread for spawn/join/yield_now.\n");

    bench_thread_create(CREATE_ITERS, WARMUP_ITERS);
    bench_thread_switch(SWITCH_ITERS, WARMUP_ITERS);

    println!("=== thread performance benchmark complete ===");
}

fn spawn_bench_thread<F, T>(f: F) -> thread::JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    thread::Builder::new()
        .stack_size(THREAD_STACK_SIZE)
        .spawn(f)
        .expect("failed to spawn benchmark thread")
}

fn run_create_join_loop(iters: u64) {
    for _ in 0..iters {
        spawn_bench_thread(|| {}).join().unwrap();
    }
}

fn bench_thread_create(iters: u64, warmup: u64) {
    if warmup > 0 {
        run_create_join_loop(warmup);
    }

    let start = Instant::now();
    run_create_join_loop(iters);
    let stats = BenchStats::new(iters, start.elapsed());

    println!("[thread create/join]");
    println!("iters: {}", stats.iters);
    println!("total: {:.3} ms", stats.total_ms());
    println!("avg thread::spawn + join: {:.2} ns", stats.avg_ns());
    println!(
        "avg thread::spawn + join: {:.3} us\n",
        stats.avg_ns() / 1_000.0
    );
}

fn bench_thread_switch(iters: u64, warmup: u64) {
    let total_iters = warmup + iters;
    let turn = Arc::new(AtomicUsize::new(0));
    let ready = Arc::new(AtomicBool::new(false));

    let worker_turn = Arc::clone(&turn);
    let worker_ready = Arc::clone(&ready);
    let worker = spawn_bench_thread(move || {
        worker_ready.store(true, Ordering::Release);

        for _ in 0..total_iters {
            wait_until(&worker_turn, 1);
            worker_turn.store(0, Ordering::Release);
        }
    });

    while !ready.load(Ordering::Acquire) {
        thread::yield_now();
    }

    for _ in 0..warmup {
        do_one_pingpong_round(&turn);
    }

    let start = Instant::now();
    for _ in 0..iters {
        do_one_pingpong_round(&turn);
    }
    let stats = BenchStats::new(iters, start.elapsed());

    worker.join().unwrap();

    let roundtrip_ns = stats.avg_ns();
    let switch_ns = estimated_switch_ns(stats);

    println!("[thread yield ping-pong]");
    println!("iters: {}", stats.iters);
    println!("total: {:.3} ms", stats.total_ms());
    println!("avg round-trip A->B->A: {roundtrip_ns:.2} ns");
    println!("estimated avg one context switch: {switch_ns:.2} ns");
    println!(
        "estimated avg one context switch: {:.3} us\n",
        switch_ns / 1_000.0
    );
}

fn do_one_pingpong_round(turn: &AtomicUsize) {
    turn.store(1, Ordering::Release);
    wait_until(turn, 0);
}

fn wait_until(turn: &AtomicUsize, expected: usize) {
    while turn.load(Ordering::Acquire) != expected {
        thread::yield_now();
    }
}
