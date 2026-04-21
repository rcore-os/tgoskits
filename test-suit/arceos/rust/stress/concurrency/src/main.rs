#![cfg_attr(feature = "ax-std", no_std)]
#![cfg_attr(feature = "ax-std", no_main)]

#[macro_use]
#[cfg(feature = "ax-std")]
extern crate ax_std as std;

#[cfg(feature = "ax-std")]
use std::os::arceos::{
    api::task::{AxCpuMask, ax_set_current_affinity},
    modules::ax_task::WaitQueue,
};
#[cfg(feature = "ax-std")]
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
    vec::Vec,
};

#[cfg(feature = "ax-std")]
const START_TIMEOUT_MS: u64 = 180;
#[cfg(feature = "ax-std")]
const RELEASE_TIMEOUT_MS: u64 = 180;
#[cfg(feature = "ax-std")]
const ROUND_TIMEOUT_MS: u64 = 2500;
#[cfg(feature = "ax-std")]
const WATCHDOG_SLEEP_MS: u64 = 50;
#[cfg(feature = "ax-std")]
const WATCHDOG_STALL_TICKS: usize = 60;
#[cfg(feature = "ax-std")]
const STAGE_ARMED_READY: usize = 1;
#[cfg(feature = "ax-std")]
const STAGE_START_RELEASED: usize = 2;
#[cfg(feature = "ax-std")]
const STAGE_MIDWAY_READY: usize = 3;
#[cfg(feature = "ax-std")]
const STAGE_RELEASED: usize = 4;
#[cfg(feature = "ax-std")]
const STAGE_FINISHED: usize = 5;

#[cfg(feature = "ax-std")]
struct Shared {
    round_hits: usize,
    critical_sum: usize,
}

#[cfg(feature = "ax-std")]
struct StressContext {
    start_round: AtomicUsize,
    release_round: AtomicUsize,
    armed_workers: AtomicUsize,
    midway_workers: AtomicUsize,
    finished_workers: AtomicUsize,
    stage_seq: AtomicUsize,
    stage_round: AtomicUsize,
    stage_kind: AtomicUsize,
    stop_watchdog: AtomicBool,
    start_wq: WaitQueue,
    release_wq: WaitQueue,
    finished_wq: WaitQueue,
    arm_wq: WaitQueue,
    shared: Mutex<Shared>,
}

#[cfg(feature = "ax-std")]
impl StressContext {
    fn new() -> Self {
        Self {
            start_round: AtomicUsize::new(0),
            release_round: AtomicUsize::new(0),
            armed_workers: AtomicUsize::new(0),
            midway_workers: AtomicUsize::new(0),
            finished_workers: AtomicUsize::new(0),
            stage_seq: AtomicUsize::new(0),
            stage_round: AtomicUsize::new(0),
            stage_kind: AtomicUsize::new(0),
            stop_watchdog: AtomicBool::new(false),
            start_wq: WaitQueue::new(),
            release_wq: WaitQueue::new(),
            finished_wq: WaitQueue::new(),
            arm_wq: WaitQueue::new(),
            shared: Mutex::new(Shared {
                round_hits: 0,
                critical_sum: 0,
            }),
        }
    }

    fn record_stage(&self, round: usize, stage_kind: usize) {
        self.stage_round.store(round, Ordering::Release);
        self.stage_kind.store(stage_kind, Ordering::Release);
        self.stage_seq.fetch_add(1, Ordering::AcqRel);
    }
}

#[cfg(feature = "ax-std")]
fn cpu_mask(cpu_num: usize, home_cpu: usize, mode: usize) -> AxCpuMask {
    let mut mask = AxCpuMask::new();
    match mode {
        0 => {
            mask.set(home_cpu, true);
        }
        1 => {
            for cpu in 0..cpu_num {
                if cpu != home_cpu {
                    mask.set(cpu, true);
                }
            }
            if mask.is_empty() {
                mask.set(home_cpu, true);
            }
        }
        _ => {
            for cpu in 0..cpu_num {
                mask.set(cpu, true);
            }
        }
    }
    mask
}

#[cfg(feature = "ax-std")]
fn set_affinity_for_round(worker: usize, round: usize, cpu_num: usize) {
    let home_cpu = worker % cpu_num;
    let mask = cpu_mask(cpu_num, home_cpu, round % 3);
    assert!(
        ax_set_current_affinity(mask).is_ok(),
        "worker {worker} failed to update affinity at round {round}",
    );
}

#[cfg(feature = "ax-std")]
fn sleep_or_yield(delay: Duration) {
    if delay.is_zero() {
        thread::yield_now();
    } else {
        thread::sleep(delay);
    }
}

#[cfg(feature = "ax-std")]
fn post_notify_disturb(round: usize, phase: usize) {
    let mix = round + phase;
    if mix.is_multiple_of(2) {
        thread::yield_now();
    }
    if mix.is_multiple_of(3) {
        thread::sleep(Duration::ZERO);
    }
    spin_for(160 + (mix % 3) * 128);
    if mix.is_multiple_of(5) {
        thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(feature = "ax-std")]
fn wait_for_counter(
    round: usize,
    worker_count: usize,
    counter: &AtomicUsize,
    wq: &WaitQueue,
    label: &str,
) {
    let expected = worker_count * (round + 1);
    let timeout = wq.wait_timeout_until(Duration::from_millis(ROUND_TIMEOUT_MS), || {
        counter.load(Ordering::Acquire) >= expected
    });
    assert!(
        !timeout,
        "round {round}: timed out waiting workers to {label}, {label}={}",
        counter.load(Ordering::Relaxed),
    );
}

#[cfg(feature = "ax-std")]
fn signal_progress(ctx: &StressContext, counter: &AtomicUsize, wq: &WaitQueue) {
    counter.fetch_add(1, Ordering::Release);
    wq.notify_one(true);
    let _ = ctx;
}

#[cfg(feature = "ax-std")]
fn wait_for_round(
    ctx: &StressContext,
    round: usize,
    gate: &AtomicUsize,
    wq: &WaitQueue,
    timeout_ms: u64,
) {
    while gate.load(Ordering::Acquire) <= round {
        let _timed_out = wq.wait_timeout_until(Duration::from_millis(timeout_ms), || {
            gate.load(Ordering::Acquire) > round
        });
        let _ = ctx;
    }
}

#[cfg(feature = "ax-std")]
fn spin_for(iterations: usize) {
    for _ in 0..iterations {
        core::hint::spin_loop();
    }
}

#[cfg(feature = "ax-std")]
fn run_first_sections(ctx: &StressContext, worker: usize, round: usize, local_score: &mut usize) {
    let sections = 2 + (worker + round) % 3;
    for section in 0..sections {
        let mix = worker + round + section;
        let mut guard = ctx.shared.lock();
        guard.round_hits += 1;
        guard.critical_sum += worker ^ round ^ section;
        *local_score = (*local_score).wrapping_add(guard.critical_sum);
        if mix.is_multiple_of(2) {
            thread::yield_now();
        }
        if mix.is_multiple_of(4) {
            spin_for(128);
        }
        drop(guard);
        sleep_or_yield(if mix.is_multiple_of(3) {
            Duration::from_millis(1)
        } else {
            Duration::ZERO
        });
    }
}

#[cfg(feature = "ax-std")]
fn run_second_sections(
    ctx: &StressContext,
    worker: usize,
    round: usize,
    cpu_num: usize,
    local_score: &mut usize,
) {
    let sections = 3 + (worker + round) % 4;
    for section in 0..sections {
        let mix = worker + round + section;
        set_affinity_for_round(worker + section + round, round + section, cpu_num);
        let mut guard = ctx.shared.lock();
        guard.round_hits += 1;
        guard.critical_sum += (worker + section) ^ (round << 1);
        *local_score = (*local_score).wrapping_add(guard.critical_sum);
        if !mix.is_multiple_of(2) {
            thread::yield_now();
        }
        if mix.is_multiple_of(5) {
            spin_for(256);
        }
        drop(guard);
        sleep_or_yield(if mix.is_multiple_of(2) {
            Duration::from_millis(1)
        } else {
            Duration::ZERO
        });
    }
}

#[cfg(feature = "ax-std")]
fn spawn_watchdog(ctx: Arc<StressContext>) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut last_stage_seq = ctx.stage_seq.load(Ordering::Acquire);
        let mut last_stage_change = Instant::now();
        while !ctx.stop_watchdog.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(WATCHDOG_SLEEP_MS));
            let stage_seq = ctx.stage_seq.load(Ordering::Acquire);
            if stage_seq != last_stage_seq {
                last_stage_seq = stage_seq;
                last_stage_change = Instant::now();
            } else {
                assert!(
                    last_stage_change.elapsed()
                        < Duration::from_millis(WATCHDOG_SLEEP_MS * WATCHDOG_STALL_TICKS as u64),
                    "watchdog detected stalled stage: stage_round={}, stage_kind={}, \
                     start_round={}, release_round={}, armed={}, midway={}, finished={}",
                    ctx.stage_round.load(Ordering::Relaxed),
                    ctx.stage_kind.load(Ordering::Relaxed),
                    ctx.start_round.load(Ordering::Relaxed),
                    ctx.release_round.load(Ordering::Relaxed),
                    ctx.armed_workers.load(Ordering::Relaxed),
                    ctx.midway_workers.load(Ordering::Relaxed),
                    ctx.finished_workers.load(Ordering::Relaxed),
                );
            }
        }
    })
}

#[cfg(feature = "ax-std")]
fn spawn_workers(
    ctx: &Arc<StressContext>,
    worker_count: usize,
    rounds: usize,
    cpu_num: usize,
) -> Vec<thread::JoinHandle<usize>> {
    (0..worker_count)
        .map(|worker| {
            let ctx = Arc::clone(ctx);
            thread::spawn(move || {
                let mut local_score = 0usize;
                for round in 0..rounds {
                    set_affinity_for_round(worker, round, cpu_num);
                    signal_progress(&ctx, &ctx.armed_workers, &ctx.arm_wq);
                    wait_for_round(
                        &ctx,
                        round,
                        &ctx.start_round,
                        &ctx.start_wq,
                        START_TIMEOUT_MS,
                    );

                    run_first_sections(&ctx, worker, round, &mut local_score);
                    signal_progress(&ctx, &ctx.midway_workers, &ctx.arm_wq);
                    wait_for_round(
                        &ctx,
                        round,
                        &ctx.release_round,
                        &ctx.release_wq,
                        RELEASE_TIMEOUT_MS,
                    );

                    run_second_sections(&ctx, worker, round, cpu_num, &mut local_score);
                    signal_progress(&ctx, &ctx.finished_workers, &ctx.finished_wq);
                }
                local_score
            })
        })
        .collect()
}

#[cfg(feature = "ax-std")]
fn run_stress() {
    let cpu_num = thread::available_parallelism().unwrap().get();
    if cpu_num <= 1 {
        println!("skip concurrency: single CPU");
        return;
    }

    let worker_count = cpu_num * 4;
    let rounds = 300;
    println!("concurrency: cpu_num={cpu_num}, worker_count={worker_count}, rounds={rounds}");

    let ctx = Arc::new(StressContext::new());
    let watchdog = spawn_watchdog(Arc::clone(&ctx));
    let workers = spawn_workers(&ctx, worker_count, rounds, cpu_num);

    for round in 0..rounds {
        wait_for_counter(round, worker_count, &ctx.armed_workers, &ctx.arm_wq, "arm");
        ctx.record_stage(round, STAGE_ARMED_READY);
        sleep_or_yield(match round % 4 {
            0 => Duration::ZERO,
            1 => Duration::from_millis(START_TIMEOUT_MS / 4),
            2 => Duration::from_millis(START_TIMEOUT_MS.saturating_sub(2)),
            _ => Duration::from_millis(START_TIMEOUT_MS + 1),
        });
        ctx.start_round.store(round + 1, Ordering::Release);
        ctx.start_wq.notify_all(true);
        post_notify_disturb(round, 0);
        ctx.record_stage(round, STAGE_START_RELEASED);

        wait_for_counter(
            round,
            worker_count,
            &ctx.midway_workers,
            &ctx.arm_wq,
            "reach midway",
        );
        ctx.record_stage(round, STAGE_MIDWAY_READY);
        sleep_or_yield(match round % 5 {
            0 => Duration::ZERO,
            1 => Duration::from_millis(RELEASE_TIMEOUT_MS / 3),
            2 => Duration::from_millis(RELEASE_TIMEOUT_MS.saturating_sub(2)),
            3 => Duration::from_millis(RELEASE_TIMEOUT_MS + 1),
            _ => Duration::from_millis(RELEASE_TIMEOUT_MS / 2),
        });
        ctx.release_round.store(round + 1, Ordering::Release);
        ctx.release_wq.notify_all(true);
        post_notify_disturb(round, 1);
        ctx.record_stage(round, STAGE_RELEASED);

        wait_for_counter(
            round,
            worker_count,
            &ctx.finished_workers,
            &ctx.finished_wq,
            "finish",
        );
        ctx.record_stage(round, STAGE_FINISHED);
        let shared = ctx.shared.lock();
        let expected_min = worker_count * (round + 1) * 5;
        assert!(
            shared.round_hits >= expected_min,
            "round {round}: insufficient critical sections, hits={}, expected_min={expected_min}",
            shared.round_hits,
        );
        drop(shared);

        if round % 5 == 0 {
            println!(
                "round {round:02}: armed={}, midway={}, finished={}, start_round={}, \
                 release_round={}",
                ctx.armed_workers.load(Ordering::Relaxed),
                ctx.midway_workers.load(Ordering::Relaxed),
                ctx.finished_workers.load(Ordering::Relaxed),
                ctx.start_round.load(Ordering::Relaxed),
                ctx.release_round.load(Ordering::Relaxed),
            );
        }
    }

    let mut total_score = 0usize;
    for handle in workers {
        total_score = total_score.wrapping_add(handle.join().unwrap());
    }

    ctx.stop_watchdog.store(true, Ordering::Release);
    watchdog.join().unwrap();

    let shared = ctx.shared.lock();
    let expected_round_hits_min = worker_count * rounds * 5;
    assert!(
        shared.round_hits >= expected_round_hits_min,
        "final round hits too small: {} < {}",
        shared.round_hits,
        expected_round_hits_min,
    );
    println!(
        "concurrency: round_hits={}, critical_sum={}, total_score={}",
        shared.round_hits, shared.critical_sum, total_score
    );
}

#[cfg_attr(feature = "ax-std", unsafe(no_mangle))]
fn main() {
    #[cfg(feature = "ax-std")]
    {
        println!("Hello, concurrency test!");
        run_stress();
    }

    println!("All tests passed!");
}
