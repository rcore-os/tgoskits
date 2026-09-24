#[cfg(target_arch = "x86_64")]
const TSC_FREQ_MHZ: u64 = 4000;

pub const fn div_round(n: u64, d: u64) -> u64 {
    (n + d / 2) / d
}

#[cfg(target_arch = "x86_64")]
#[inline]
pub fn now_tsc() -> u64 {
    unsafe { core::arch::x86_64::__rdtscp(&mut 0) }
}

#[cfg(target_arch = "x86_64")]
#[inline]
#[allow(dead_code)]
pub fn now_ns() -> u64 {
    now_tsc() * 1000 / TSC_FREQ_MHZ
}

#[cfg(target_arch = "x86_64")]
pub fn ticks_to_nanos(ticks: u64) -> u64 {
    ticks * 1_000 / TSC_FREQ_MHZ
}

#[cfg(target_arch = "x86_64")]
#[inline]
pub fn timer_freq() -> u64 {
    TSC_FREQ_MHZ * 1_000_000
}

#[cfg(target_arch = "aarch64")]
use core::sync::atomic::AtomicU64;

#[cfg(target_arch = "aarch64")]
use aarch64_cpu::registers::{CNTFRQ_EL0, CNTVCT_EL0, Readable};

#[cfg(target_arch = "aarch64")]
pub static CPUFRQ_HZ: AtomicU64 = AtomicU64::new(2_400_000_000); // RK3588 CPU主频2.4GHz

#[cfg(target_arch = "aarch64")]
#[inline]
pub fn timer_freq() -> u64 {
    CNTFRQ_EL0.get()
}

#[cfg(target_arch = "aarch64")]
#[inline]
pub fn now_tsc() -> u64 {
    CNTVCT_EL0.get()
}

#[cfg(target_arch = "aarch64")]
#[inline]
#[allow(dead_code)]
pub fn now_ns() -> u64 {
    let freq = CNTFRQ_EL0.get();
    now_tsc() * (1_000_000_000 / freq)
}

#[cfg(target_arch = "aarch64")]
pub fn ticks_to_nanos(ticks: u64) -> u64 {
    let freq = CNTFRQ_EL0.get();
    ticks * (1_000_000_000 / freq)
}

pub struct Bencher {
    name: &'static str,
    count: u64,
    sum_tsc: u64,
    max_tsc: u64,
    sum_cpu_cycle: u64,
    max_cpu_cycle: u64,
    min_cpu_cycle: u64,
}

impl Bencher {
    pub fn new(name: &'static str) -> Self {
        Self {
            name,
            count: 0,
            sum_tsc: 0,
            max_tsc: 0,
            sum_cpu_cycle: 0,
            max_cpu_cycle: 0,
            min_cpu_cycle: 0,
        }
    }

    #[inline]
    #[allow(dead_code)]
    pub fn bench_fn(f: impl FnOnce()) -> u64 {
        let start = now_tsc();
        f();
        now_tsc() - start
    }

    #[inline]
    pub fn add_result(&mut self, tsc: u64) {
        self.count += 1;
        self.sum_tsc += tsc;
        self.set_max_tsc(tsc);
    }

    #[inline]
    pub fn bench_once<T>(&mut self, f: impl FnOnce() -> T) -> T {
        let start = now_tsc();
        let res = f();
        let elapsed = now_tsc() - start;
        self.add_result(elapsed);
        res
    }

    pub fn set_max_tsc(&mut self, tsc: u64) {
        if self.max_tsc < tsc {
            self.max_tsc = tsc;
        }
    }

    pub fn set_a_cpu_cycle(&mut self, cpu_cycle: u64) {
        if cpu_cycle > self.max_cpu_cycle {
            self.max_cpu_cycle = cpu_cycle;
        }
        if (cpu_cycle < self.min_cpu_cycle) || (self.min_cpu_cycle == 0) {
            self.min_cpu_cycle = cpu_cycle;
        }
    }

    pub fn bench_many<T>(&mut self, f: impl Fn() -> T, warmup: usize, run: usize) -> &mut Self {
        for _ in 0..warmup {
            let _ = f();
        }

        let start = now_tsc();
        for _ in 0..run {
            let _ = f();
        }
        let elapsed = now_tsc() - start;

        self.count += run as u64;
        self.sum_tsc += elapsed;
        self.set_max_tsc(div_round(elapsed, run as u64));
        self
    }

    // Maybe - xiaoluoyuan@163.com
    pub fn reset(&mut self, run: u64, elapsed: u64, cpu_cycle: u64) -> &mut Self {
        self.count += run;
        self.sum_tsc += elapsed;
        self.sum_cpu_cycle += cpu_cycle;

        if self.max_tsc == 0 {
            self.set_max_tsc(div_round(elapsed, run));
        }

        self
    }

    pub fn show(&self) {
        println!("\nBenchmark: {}", self.name);
        println!("  Iterations: {}", self.count);
        if self.count == 0 {
            return;
        }
        println!(
            "  Benchmark total duration: {} s",
            self.sum_tsc / timer_freq()
        );
        // println!("  Max Timer cycles: {}", self.max_tsc);
        // println!("  Average Timer cycles: {}", div_round(self.sum_tsc, self.count));

        println!(
            "  Average timer nanoseconds: {} ns",
            div_round(ticks_to_nanos(self.sum_tsc), self.count)
        );

        #[cfg(target_arch = "aarch64")]
        {
            // let timer_freq = timer_freq();
            // println!("  Average RK3588(2.4GHz) CPU cycles: {}", div_round(self.sum_tsc, self.count) * (CPUFRQ_HZ.load(core::sync::atomic::Ordering::Relaxed) / timer_freq) );

            if self.max_cpu_cycle != 0 {
                // println!("  Min CPU cycles: {}", self.min_cpu_cycle);
                println!(
                    "  Average CPU cycles: {}",
                    div_round(self.sum_cpu_cycle, self.count)
                );
                // println!("  Max CPU cycles: {}", self.max_cpu_cycle);

                let cpu_freq = crate::cycle::cpu_freq(self.sum_cpu_cycle, self.sum_tsc);
                println!(
                    "\n  CPU Freq: {} Hz ({}.{} GHz)",
                    cpu_freq,
                    cpu_freq / 1_000_000_000,
                    div_round(cpu_freq % 1_000_000_000, 1_000_000)
                );
            }
        }
    }
}

// macro_rules! bench_expr {
//     ($f:expr) => {{
//         let start = now_ns();
//         $f;
//         now_ns() - start
//     }};
// }

// macro_rules! bench {
//     ($f:expr, $name:expr, $iter:expr) => {{
//         // warmup
//         for _ in 0..10000 {
//             $f();
//         }

//         let elapsed = bench_expr!({
//             for _ in 0..$iter {
//                 $f();
//             }
//         });
//         println!("Benchmark: {}", $name);
//         println!("  Iterations: {}", $iter);
//         println!(
//             "  Elapsed: {}.{:03} s",
//             elapsed.as_secs(),
//             elapsed.subsec_millis()
//         );
//         println!("  Latency: {} ns", elapsed.as_nanos() / $iter as u128);
//     }};
// }
