//! Rust-Shyper task-switch benchmark running as an AxVisor ArceOS board guest.

// Linking `ax-std` supplies the ArceOS standard-library compatibility layer
// that the `std` workload below runs on.
#[cfg(feature = "arceos")]
use ax_std as _;

#[cfg(target_arch = "aarch64")]
mod cycle;

#[cfg(not(feature = "qemu"))]
mod gpio;

#[macro_use]
#[path = "bencher.rs"]
mod bencher;
use std::thread;

use bencher::*;

/// FIFO priority for the benchmark's task-switch workload.
///
/// Only the dedicated board benchmark build enables `bench-fifo-policy`; boot,
/// `rdtsc`, and `spawn` keep the runtime's ordinary Fair default policy, and
/// the runtime-wide `SchedulePolicy::default()` is not changed.
#[cfg(feature = "bench-fifo-policy")]
const BENCH_SWITCH_FIFO_PRIORITY: u8 = 80;

#[cfg(feature = "bench-fifo-policy")]
fn bench_switch_fifo_policy() -> ax_runtime::task::sched::SchedulePolicy {
    let priority = ax_runtime::task::sched::RtPriority::new(BENCH_SWITCH_FIFO_PRIORITY)
        .expect("BENCHER_FIFO_FAILED: invalid task-switch FIFO priority");
    ax_runtime::task::sched::SchedulePolicy::fifo(priority)
}

/// Moves the already-initialized bencher task into the FIFO class.
///
/// A failure panics before the measured rounds start, so the benchmark can
/// never print its `Bencher end` success marker without the requested policy.
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

fn bench_spawn() {
    let warmup = 0;
    let iter = if cfg!(feature = "arceos") {
        500_000
    } else {
        200_000
    };

    let mut b = Bencher::new("spawn");
    for _ in 0..warmup {
        let t = thread::spawn(|| {});
        t.join().unwrap();
    }

    for _ in 0..iter {
        b.bench_once(|| thread::spawn(|| {})).join().unwrap();
    }
    b.show();

    // b.reset(0, 0);
    // for _ in 0..iter {
    //     b.bench_once(|| thread::spawn(|| {}).join()).unwrap();
    // }
    // b.show();
}

/// 创建两个线程，每次yield都会切到另一线程;
/// 每个线程分别yield (iter/2)次;
/// 单次yield的时间 = 总时间/iter
fn bench_switch(iter: u64) {
    let peer = move || {
        for _i in 0..iter / 2 {
            // println!("1 THREAD, switch {}", i);

            // GPIO Output： 低电平
            #[cfg(not(feature = "qemu"))]
            {
                gpio::gpio3_output_low();
            }

            thread::yield_now();
        }
    };

    // The peer is intentionally never joined, exactly like the original
    // `thread::spawn` statement: it leaves the CPU after its own yield loop.
    #[cfg(feature = "bench-fifo-policy")]
    let _peer = spawn_switch_peer(peer);
    #[cfg(not(feature = "bench-fifo-policy"))]
    thread::spawn(peer);

    let mut bencher_switch = Bencher::new("switch");
    let mut sum_cpu_cycle = 0;
    let mut sum_tsc = 0;

    #[cfg(not(feature = "qemu"))]
    {
        println!("2 THREADS switching GPIO3_C6 output between low and high");
        gpio::gpio3_output_low();
        gpio::gpio3_output_high();
        gpio::gpio3_output_low();
        gpio::gpio3_output_high();
    }
    // println!("Start the task thread switching test ...");

    for _i in 0..iter / 2 {
        // println!("0 THREAD, switch {}", i);

        let tsc_start = now_tsc();
        let cpu_cycle_start = cycle::cpu_cycle();

        // 首先运行；当前任务主动放弃CPU使用，主动切换到另一个就绪的任务
        thread::yield_now();

        let cpu_cycle_end = cycle::cpu_cycle();
        let tsc_end = now_tsc();

        // GPIO Output 高电平
        #[cfg(not(feature = "qemu"))]
        {
            gpio::gpio3_output_high();
        }

        let cpu_cycle = cpu_cycle_end - cpu_cycle_start;
        let tsc = tsc_end - tsc_start;
        sum_cpu_cycle += cpu_cycle;
        sum_tsc += tsc;
        bencher_switch.set_a_cpu_cycle(cpu_cycle / 2);
        bencher_switch.set_max_tsc(tsc / 2);
    }

    bencher_switch.reset(iter, sum_tsc, sum_cpu_cycle).show();
}

fn main() {
    #[cfg(not(feature = "qemu"))]
    {
        // The benchmark drives a GPIO3_C6 pulse inside every timed switch
        // interval. Stop before measuring anything when that pin cannot be set
        // up, so the board case cannot pass without the physical signal.
        if let Err(reason) = gpio::init() {
            println!("BENCHER_GPIO_FAILED reason={reason}");
            return;
        }
        gpio::gpio3_led_red_on();
    }

    println!("Bencher start ...\n");

    // User access PMU
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

    // The condition-variable benchmark stays disabled, exactly like the
    // original source: this workload measures `rdtsc`, `spawn`, and the
    // 30 x 10,000,000 task-switch rounds only.

    let cpu_cycle_end = cycle::cpu_cycle();
    let timer_end = cycle::timer_cnt();

    let cpu_cycle = cpu_cycle_end - cpu_cycle_start;
    let timer_sum = timer_end - timer_start;

    let timer_freq = cycle::timer_freq();
    let s_sum = timer_sum / timer_freq;
    let ns_sum = timer_sum * (1_000_000_000 / timer_freq);

    let cpu_freq = cycle::cpu_freq(cpu_cycle, timer_sum);
    CPUFRQ_HZ.store(cpu_freq, core::sync::atomic::Ordering::Relaxed);

    println!(
        "\nCPU Freq = {}Hz, CPU Cycle Counter = {} from {} to {}, In {}s, {}ns",
        cpu_freq, cpu_cycle, cpu_cycle_start, cpu_cycle_end, s_sum, ns_sum
    );

    // 评测task调度切换开销
    println!("\nBencher: task switch ...");
    #[cfg(target_arch = "aarch64")]
    println!(
        "AARCH64 Generic Timer Registers: CNTFRQ_EL0={}, CNTVCT_EL0={}",
        timer_freq,
        now_tsc()
    );

    // 评测切换的次数

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

    #[cfg(not(feature = "qemu"))]
    {
        gpio::gpio3_clear_all();
        gpio::gpio3_led_green_on();

        gpio::gpio3_ver_id_get();
        gpio::gpio3_ext_port_signals_get();
    }

    // 每1000万次切换将输出一次GPIO UART信号
    println!("After every 10 million task switches, a GPIO UART signal will be output");
    let switch_count = 10_000_000;
    let iter = 30;

    // Scope FIFO to the task-switch workload: the runtime initialized, booted,
    // and ran `rdtsc`/`spawn` under its ordinary Fair default policy.
    #[cfg(feature = "bench-fifo-policy")]
    configure_current_task_for_switch_bench();

    for i in 0..iter {
        println!(
            "\n---------\nBencher: {} task switch count = {}",
            i, switch_count
        );

        // #[cfg(not(feature = "qemu"))]
        // {
        // gpio::uart7_put_hi();
        // }

        bench_switch(switch_count);
    }

    #[cfg(not(feature = "qemu"))]
    {
        gpio::gpio3_clear_all();
        gpio::gpio3_led_red_on();
    }

    println!("\nBencher end");
}
