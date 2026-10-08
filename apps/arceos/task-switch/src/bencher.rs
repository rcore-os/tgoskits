//! Task-switch benchmark harness.
//!
//! [`Bencher`] accumulates raw System Counter ticks (`CNTVCT_EL0`) and PMU cycle
//! counts. The tick/frequency unit conversions live in [`crate::convert`] so
//! they can be exercised without reading hardware registers.

use aarch64_cpu::registers::{CNTFRQ_EL0, CNTVCT_EL0, Readable};

use crate::convert;

/// Reads the System Counter frequency (`CNTFRQ_EL0`) in Hz.
#[inline]
pub fn timer_freq() -> u64 {
    CNTFRQ_EL0.get()
}

/// Reads the System Counter (`CNTVCT_EL0`) in ticks.
#[inline]
pub fn now_tsc() -> u64 {
    CNTVCT_EL0.get()
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

    /// Smallest per-switch CPU cycle count passed to [`Bencher::set_a_cpu_cycle`].
    ///
    /// `bench_switch` stores `cpu_cycle / 2` for every round trip, so this is
    /// the fastest switch seen so far. It stays `0` until the first call records
    /// a value.
    pub fn min_cpu_cycle(&self) -> u64 {
        self.min_cpu_cycle
    }

    /// Largest per-switch CPU cycle count passed to [`Bencher::set_a_cpu_cycle`].
    ///
    /// This is the slowest switch seen so far, in the same per-switch unit as
    /// [`Bencher::min_cpu_cycle`].
    pub fn max_cpu_cycle(&self) -> u64 {
        self.max_cpu_cycle
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
        if let Some(average) = convert::div_round(elapsed, run as u64) {
            self.set_max_tsc(average);
        }
        self
    }

    // Maybe - xiaoluoyuan@163.com
    pub fn reset(&mut self, run: u64, elapsed: u64, cpu_cycle: u64) -> &mut Self {
        self.count += run;
        self.sum_tsc += elapsed;
        self.sum_cpu_cycle += cpu_cycle;

        if self.max_tsc == 0
            && let Some(average) = convert::div_round(elapsed, run)
        {
            self.set_max_tsc(average);
        }

        self
    }

    pub fn show(&self) {
        println!("\nBenchmark: {}", self.name);
        println!("  Iterations: {}", self.count);
        if self.count == 0 {
            return;
        }

        let freq = timer_freq();
        if let Some(seconds) = self.sum_tsc.checked_div(freq) {
            println!("  Benchmark total duration: {} s", seconds);
        }

        if let Some(average_ns) = convert::ticks_to_nanos(self.sum_tsc, freq)
            .and_then(|nanos| convert::div_round(nanos, self.count))
        {
            println!("  Average timer nanoseconds: {} ns", average_ns);
        }

        if self.max_cpu_cycle != 0 {
            if let Some(average_cycles) = convert::div_round(self.sum_cpu_cycle, self.count) {
                println!("  Average CPU cycles: {}", average_cycles);
            }

            if let Some(cpu_freq) = convert::cpu_freq_hz(self.sum_cpu_cycle, self.sum_tsc, freq)
                && let Some(ghz_fraction) = convert::div_round(cpu_freq % 1_000_000_000, 1_000_000)
            {
                println!(
                    "\n  CPU Freq: {} Hz ({}.{} GHz)",
                    cpu_freq,
                    cpu_freq / 1_000_000_000,
                    ghz_fraction
                );
            }
        }
    }
}
