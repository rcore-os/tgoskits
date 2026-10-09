//! Rust-Shyper task-switch benchmark running as an AxVisor ArceOS board guest.
//!
//! The application only supports the AArch64 RK3588 board guest. Its tasks run
//! at EL1: they read the PMU cycle counter and the System Counter, and drive the
//! physical GPIO3_C6 pin through direct memory-mapped I/O. The build inputs live
//! in `benchmarks/axvisor/board-orangepi-5-plus/task-switch/`.
//!
//! Only the pure counter/time conversions in [`convert`] build on their own;
//! they are what `cargo xtask cross-test --no-default-features` exercises. The
//! board program needs the `arceos` capability set on AArch64.

// Non-AArch64 targets have no supported build at all. Fail before any module
// that would pull in AArch64-only dependencies, so this is the diagnostic
// instead of a cascade of unresolved imports.
#[cfg(not(target_arch = "aarch64"))]
compile_error!(
    "task-switch only supports the AArch64 RK3588 ArceOS board guest; build it for \
     aarch64-unknown-none-softfloat as described in guest-build.toml"
);

// On AArch64 without the `arceos` capability set (and outside `cfg(test)`) the
// crate would build neither the board program nor the unit tests, so report the
// supported configurations instead of leaving the crate without an entry point.
#[cfg(all(target_arch = "aarch64", not(feature = "arceos"), not(test)))]
compile_error!(
    "task-switch's board program requires the `arceos` feature; without it only the `convert` \
     unit tests build (cargo xtask cross-test --no-default-features)"
);

// Pure counter/time conversions, the only module built without `arceos`.
mod convert;

// The board program and its hardware modules all need both the ArceOS
// standard-library guest and the AArch64-only register dependencies, so a single
// condition gates every one of them.
#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
use ax_std as _;

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
mod cycle;

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
mod gpio;

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
mod bencher;

// Only the FIFO task-switch handshake shares state across tasks.
#[cfg(all(feature = "bench-fifo-policy", target_arch = "aarch64"))]
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU8, Ordering},
};
#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
use std::thread;

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
use bencher::*;

/// FIFO priority for the benchmark's task-switch workload.
///
/// Only the dedicated board benchmark build enables `bench-fifo-policy`; boot,
/// `rdtsc`, and `spawn` keep the runtime's ordinary Fair default policy, and the
/// runtime-wide `SchedulePolicy::default()` is not changed.
#[cfg(feature = "bench-fifo-policy")]
const BENCH_SWITCH_FIFO_PRIORITY: u8 = 80;

/// Turn tokens for the FIFO task-switch handshake.
///
/// The main thread starts holding `SWITCH_TURN_MAIN`, and each timed interval
/// spans exactly one `main -> peer` and one `peer -> main` yield pair.
#[cfg(feature = "bench-fifo-policy")]
const SWITCH_TURN_MAIN: u8 = 0;
#[cfg(feature = "bench-fifo-policy")]
const SWITCH_TURN_PEER: u8 = 1;

/// Number of measured task-switch rounds, kept at the original workload.
#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
const SWITCH_ROUNDS: u64 = 30;

/// Task switches measured in every round, kept at the original workload.
#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
const SWITCHES_PER_ROUND: u64 = 10_000_000;

#[cfg(feature = "bench-fifo-policy")]
fn bench_switch_fifo_policy() -> ax_runtime::task::sched::SchedulePolicy {
    let priority = ax_runtime::task::sched::RtPriority::new(BENCH_SWITCH_FIFO_PRIORITY)
        .expect("BENCHER_FIFO_FAILED: invalid task-switch FIFO priority");
    ax_runtime::task::sched::SchedulePolicy::fifo(priority)
}

/// Moves the already-initialized bencher task into the FIFO class.
///
/// A failure panics before the measured rounds start, so the benchmark can
/// never print its success markers without the requested policy.
#[cfg(feature = "bench-fifo-policy")]
fn configure_current_task_for_switch_bench() {
    ax_runtime::task::thread::current::current_thread_handle()
        .expect("BENCHER_FIFO_FAILED: read current task handle")
        .set_policy(bench_switch_fifo_policy())
        .expect("BENCHER_FIFO_FAILED: set current task FIFO policy");
}

/// Creates the switch peer in FIFO(80) before it is published.
#[cfg(feature = "bench-fifo-policy")]
fn spawn_switch_peer(
    entry: impl FnOnce() + Send + 'static,
) -> ax_runtime::task::thread::ThreadHandle {
    ax_runtime::thread::builder(String::from("bench-switch-peer"))
        .policy(bench_switch_fifo_policy())
        .spawn(entry)
        .expect("BENCHER_FIFO_FAILED: spawn FIFO switch peer")
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
fn bench_spawn() {
    let warmup = 0;
    let iter = 500_000;

    let mut b = Bencher::new("spawn");
    for _ in 0..warmup {
        let t = thread::spawn(|| {});
        t.join().unwrap();
    }

    for _ in 0..iter {
        b.bench_once(|| thread::spawn(|| {})).join().unwrap();
    }
    b.show();
}

/// Runs one measured round of `iter` task switches.
///
/// `iter` is the switch count, so the loop performs `iter / 2` main/peer round
/// trips (each round trip is two switches). Returns
/// `(system_counter_ticks, pmu_cycle_counts, samples, min_cycles, max_cycles)`;
/// `samples` is the switch count the round reports, matching `Bencher::reset`'s
/// count, while `min_cycles`/`max_cycles` are the fastest and slowest per-switch
/// PMU cycle counts observed in the round.
#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
fn bench_switch(iter: u64) -> (u64, u64, u64, u64, u64) {
    // FIFO handshake state: the main thread owns the initial turn, and the
    // peer publishes readiness before it starts waiting.
    #[cfg(feature = "bench-fifo-policy")]
    let turn = Arc::new(AtomicU8::new(SWITCH_TURN_MAIN));
    #[cfg(feature = "bench-fifo-policy")]
    let ready = Arc::new(AtomicBool::new(false));
    #[cfg(feature = "bench-fifo-policy")]
    let peer_turn = Arc::clone(&turn);
    #[cfg(feature = "bench-fifo-policy")]
    let peer_ready = Arc::clone(&ready);

    let peer = move || {
        #[cfg(feature = "bench-fifo-policy")]
        peer_ready.store(true, Ordering::Release);

        for _i in 0..iter / 2 {
            // The peer only toggles the pin and hands the turn back once the
            // main thread has published this task's turn.
            #[cfg(feature = "bench-fifo-policy")]
            while peer_turn.load(Ordering::Acquire) != SWITCH_TURN_PEER {
                thread::yield_now();
            }

            // GPIO output: low level.
            gpio::gpio3_output_low();

            #[cfg(feature = "bench-fifo-policy")]
            peer_turn.store(SWITCH_TURN_MAIN, Ordering::Release);

            thread::yield_now();
        }
    };

    // Non-FIFO keeps the original spawn position, before the board preamble.
    #[cfg(not(feature = "bench-fifo-policy"))]
    thread::spawn(peer);

    let mut bencher_switch = Bencher::new("switch");
    let mut sum_cpu_cycle = 0;
    let mut sum_tsc = 0;

    println!("2 THREADS switching GPIO3_C6 output between low and high");
    gpio::gpio3_output_low();
    gpio::gpio3_output_high();
    gpio::gpio3_output_low();
    gpio::gpio3_output_high();

    // Spawn the FIFO peer only after the board preamble, so it cannot spin-wait
    // for its turn while potentially blocking console output runs. The peer is
    // intentionally never joined, exactly like the original `thread::spawn`
    // statement: it leaves the CPU after its own yield loop.
    #[cfg(feature = "bench-fifo-policy")]
    let _peer = spawn_switch_peer(peer);

    // Do not start measuring before the peer task is actually scheduled, so
    // early rounds cannot run ahead of a not-yet-ready peer.
    #[cfg(feature = "bench-fifo-policy")]
    while !ready.load(Ordering::Acquire) {
        thread::yield_now();
    }

    for _i in 0..iter / 2 {
        // Wait outside the measured interval until the peer has handed the turn
        // back, so the interval covers a full main->peer, peer->main pair.
        #[cfg(feature = "bench-fifo-policy")]
        while turn.load(Ordering::Acquire) != SWITCH_TURN_MAIN {
            thread::yield_now();
        }

        let tsc_start = now_tsc();
        let cpu_cycle_start = cycle::cpu_cycle();

        #[cfg(feature = "bench-fifo-policy")]
        turn.store(SWITCH_TURN_PEER, Ordering::Release);

        // The current task yields the CPU, switching to the other ready task.
        thread::yield_now();

        #[cfg(feature = "bench-fifo-policy")]
        while turn.load(Ordering::Acquire) != SWITCH_TURN_MAIN {
            thread::yield_now();
        }

        let cpu_cycle_end = cycle::cpu_cycle();
        let tsc_end = now_tsc();

        // GPIO output: high level.
        gpio::gpio3_output_high();

        let cpu_cycle = cpu_cycle_end - cpu_cycle_start;
        let tsc = tsc_end - tsc_start;
        sum_cpu_cycle += cpu_cycle;
        sum_tsc += tsc;
        bencher_switch.set_a_cpu_cycle(cpu_cycle / 2);
        bencher_switch.set_max_tsc(tsc / 2);
    }

    let min_cycles = bencher_switch.min_cpu_cycle();
    let max_cycles = bencher_switch.max_cpu_cycle();
    bencher_switch.reset(iter, sum_tsc, sum_cpu_cycle).show();
    (sum_tsc, sum_cpu_cycle, iter, min_cycles, max_cycles)
}

#[cfg(all(feature = "arceos", target_arch = "aarch64"))]
fn main() {
    // The benchmark drives a GPIO3_C6 pulse inside every timed switch interval.
    // Stop before measuring anything when that pin cannot be set up, so the
    // board case cannot pass without the physical signal.
    if let Err(reason) = gpio::init() {
        println!("BENCHER_GPIO_FAILED reason={reason}");
        return;
    }
    gpio::gpio3_led_red_on();

    println!("Bencher start ...\n");

    // Set the EL0 access bit for parity, then clear and enable the counters;
    // this workload reads the PMU registers at EL1.
    cycle::enable_cpu_cycle();
    cycle::reset_pmu_all();
    cycle::enable_pmu_all();
    cycle::isb();

    let timer_start = cycle::timer_cnt();
    let cpu_cycle_start = cycle::cpu_cycle();

    Bencher::new("rdtsc")
        .bench_many(now_tsc, 10000, 100_000_000)
        .show();

    bench_spawn();

    // This workload measures `rdtsc`, `spawn`, and the 30 x 10,000,000
    // task-switch rounds only; the condition-variable benchmark stays disabled.
    let cpu_cycle_end = cycle::cpu_cycle();
    let timer_end = cycle::timer_cnt();

    let cpu_cycle = cpu_cycle_end - cpu_cycle_start;
    let timer_sum = timer_end - timer_start;
    let timer_freq = cycle::timer_freq();

    let Some(s_sum) = timer_sum.checked_div(timer_freq) else {
        println!("BENCHER_MEASUREMENT_FAILED reason=timer-divide");
        return;
    };
    let Some(ns_sum) = convert::ticks_to_nanos(timer_sum, timer_freq) else {
        println!("BENCHER_MEASUREMENT_FAILED reason=timer-conversion");
        return;
    };
    let Some(cpu_freq) = convert::cpu_freq_hz(cpu_cycle, timer_sum, timer_freq) else {
        println!("BENCHER_MEASUREMENT_FAILED reason=cpu-freq-conversion");
        return;
    };

    println!(
        "\nCPU Freq = {}Hz, CPU Cycle Counter = {} from {} to {}, In {}s, {}ns",
        cpu_freq, cpu_cycle, cpu_cycle_start, cpu_cycle_end, s_sum, ns_sum
    );

    println!("\nBencher: task switch ...");
    println!(
        "AARCH64 Generic Timer Registers: CNTFRQ_EL0={}, CNTVCT_EL0={}",
        timer_freq,
        now_tsc()
    );

    cycle::isb();
    println!(
        "CPU{} cycle={}, timer cnt={}",
        cycle::get_cpu_id(),
        cycle::cpu_cycle(),
        cycle::timer_cnt()
    );
    println!(
        "PMUSERENR_EL0={:#x}, PMCNTENSET_EL0={:#x}, PMCR_EL0={:#x}",
        cycle::armv8_pmuserenr(),
        cycle::armv8_pmcntenset(),
        cycle::armv8_pmcr()
    );

    gpio::gpio3_clear_all();
    gpio::gpio3_led_green_on();
    gpio::gpio3_ver_id_get();
    gpio::gpio3_ext_port_signals_get();

    println!("After every 10 million task switches, a GPIO UART signal will be output");

    // Scope FIFO to the task-switch workload: the runtime initialized, booted,
    // and ran `rdtsc`/`spawn` under its ordinary Fair default policy.
    #[cfg(feature = "bench-fifo-policy")]
    configure_current_task_for_switch_bench();

    let mut avg_ns_sum: u128 = 0;
    let mut avg_cycles_sum: u128 = 0;

    for round in 0..SWITCH_ROUNDS {
        println!(
            "\n---------\nBencher: {} task switch count = {}",
            round, SWITCHES_PER_ROUND
        );

        let (round_tsc, round_cpu_cycle, round_samples, round_min_cycles, round_max_cycles) =
            bench_switch(SWITCHES_PER_ROUND);

        let Some(round_avg_cycles) = convert::div_round(round_cpu_cycle, round_samples) else {
            println!("BENCHER_MEASUREMENT_FAILED reason=round-cycles round={round}");
            return;
        };
        let Some(round_nanos) = convert::ticks_to_nanos(round_tsc, timer_freq) else {
            println!("BENCHER_MEASUREMENT_FAILED reason=round-nanos round={round}");
            return;
        };
        let Some(round_avg_ns) = convert::div_round(round_nanos, round_samples) else {
            println!("BENCHER_MEASUREMENT_FAILED reason=round-avg-ns round={round}");
            return;
        };
        if round_avg_cycles == 0 || round_avg_ns == 0 {
            println!("BENCHER_MEASUREMENT_FAILED reason=zero-measurement round={round}");
            return;
        }

        // Legacy performance-report marker. `scripts/test/ci_perf_report.py`
        // keys the nightly `task-switch/avg_cycles/index-<n>` metric on this
        // line, so the prefix and field names stay as they were when the
        // benchmark ran as the Rust-Shyper guest. `samples_per_direction`
        // counts the switches in one direction (`iter / 2`), matching the
        // original guest's `min(main_to_child, child_to_main)` reporting, and
        // `min_cycles`/`max_cycles` are the round's per-switch extremes.
        println!(
            "AXVISOR_TASK_SWITCH_GROUP_SUMMARY index={} samples_per_direction={} avg_cycles={} \
             min_cycles={} max_cycles={}",
            round,
            round_samples / 2,
            round_avg_cycles,
            round_min_cycles,
            round_max_cycles,
        );

        avg_ns_sum += u128::from(round_avg_ns);
        avg_cycles_sum += u128::from(round_avg_cycles);
    }

    let rounds = u128::from(SWITCH_ROUNDS);
    let Some(avg_ns) = u64::try_from(avg_ns_sum / rounds).ok() else {
        println!("BENCHER_MEASUREMENT_FAILED reason=avg-ns-range");
        return;
    };
    let Some(avg_cycles) = u64::try_from(avg_cycles_sum / rounds).ok() else {
        println!("BENCHER_MEASUREMENT_FAILED reason=avg-cycles-range");
        return;
    };
    if avg_ns == 0 || avg_cycles == 0 {
        println!("BENCHER_MEASUREMENT_FAILED reason=zero-average");
        return;
    }

    gpio::gpio3_clear_all();
    gpio::gpio3_led_red_on();

    println!(
        "TASK_SWITCH_SUMMARY rounds={} samples_per_round={} avg_ns={} avg_cycles={}",
        SWITCH_ROUNDS, SWITCHES_PER_ROUND, avg_ns, avg_cycles
    );
    println!("\nBencher end");
}
