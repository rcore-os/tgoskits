//! ARMv8 PMUv3 cycle-counter access layer.
//!
//! This is the cycle-counter-only slice of hardware-PMU `perf` support. It
//! exposes the dedicated 64-bit cycle counter (`PMCCNTR_EL0`) and the minimal
//! global state needed to make it tick: probing whether PMUv3 is implemented,
//! per-CPU global enable (`PMCR_EL0.E`), and a self-check that guards against
//! firmware / `MDCR_EL2` configurations that silently keep the counter frozen.
//!
//! Register access uses plain inline assembly (`mrs`/`msr`) in the same style as
//! [`super::asm`]; the named system registers used here are accepted directly by
//! the assembler.

use core::arch::asm;

/// Information probed from the PMU.
pub struct PmuInfo {
    /// `PMCR_EL0.N`: number of programmable event counters.
    ///
    /// The dedicated cycle counter (`PMCCNTR_EL0`) is separate and not included
    /// in this count.
    pub num_counters: usize,
}

/// Reads `ID_AA64DFR0_EL1` (debug feature register 0).
#[inline]
fn read_id_aa64dfr0_el1() -> u64 {
    let value;
    unsafe {
        asm!("mrs {}, ID_AA64DFR0_EL1", out(reg) value);
    }
    value
}

/// Reads `PMCR_EL0` (performance monitors control register).
#[inline]
fn read_pmcr_el0() -> u64 {
    let value;
    unsafe {
        asm!("mrs {}, PMCR_EL0", out(reg) value);
    }
    value
}

/// Writes `PMCR_EL0` (performance monitors control register).
#[inline]
fn write_pmcr_el0(value: u64) {
    unsafe {
        asm!("msr PMCR_EL0, {}", in(reg) value);
    }
}

/// Returns the raw `ID_AA64DFR0_EL1.PMUVer` field (bits `[11:8]`).
#[inline]
fn pmu_version() -> u64 {
    (read_id_aa64dfr0_el1() >> 8) & 0xf
}

/// Returns whether PMUv3 is implemented.
///
/// `PMUVer` of `0` means not implemented and `0xF` is the IMPLEMENTATION
/// DEFINED form (no PMUv3 system registers), so PMUv3 is present iff the field
/// is in `1..=0xE`.
#[inline]
fn pmu_present() -> bool {
    let v = pmu_version();
    v >= 1 && v != 0xF
}

/// Probes the PMU.
///
/// Returns `Some(PmuInfo)` iff PMUv3 is implemented
/// (`ID_AA64DFR0_EL1.PMUVer` in `1..=0xE`), else `None`.
pub fn probe() -> Option<PmuInfo> {
    if !pmu_present() {
        return None;
    }

    // PMCR_EL0.N: bits [15:11], number of programmable event counters.
    let num_counters = ((read_pmcr_el0() >> 11) & 0x1f) as usize;
    Some(PmuInfo { num_counters })
}

/// Per-CPU one-time init: set `PMCR_EL0.E` (global counter enable).
///
/// Idempotent and safe to call on each CPU. No-op if [`probe`] returns `None`.
pub fn init_cpu() {
    if !pmu_present() {
        return;
    }

    // PMCR_EL0.E (bit 0): enable all counters.
    let pmcr = read_pmcr_el0();
    write_pmcr_el0(pmcr | (1 << 0));
}

/// Reads the raw `MIDR_EL1` (main ID register).
///
/// The implementer / part fields identify the cluster a CPU belongs to and back
/// the `/proc/cpuinfo` view.
pub fn read_midr_el1() -> u64 {
    let value;
    unsafe {
        asm!("mrs {}, MIDR_EL1", out(reg) value);
    }
    value
}

/// Self-check guarding against firmware / `MDCR_EL2` issues that keep the cycle
/// counter frozen.
///
/// Configures and enables the cycle counter, spins a short volatile loop, and
/// returns `true` iff `PMCCNTR_EL0` advanced. A `false` result indicates the
/// counter is not actually counting (e.g. disabled at a higher EL).
pub fn self_check() -> bool {
    cycles::configure(false, false);
    cycles::enable();

    let a = cycles::read();
    // Short volatile spin so the counter has cycles to advance. `black_box`
    // prevents the loop from being optimized away.
    for _ in 0..100_000u32 {
        core::hint::black_box(());
    }
    let b = cycles::read();

    b > a
}

/// The dedicated 64-bit cycle counter (`PMCCNTR_EL0`).
pub mod cycles {
    use core::arch::asm;

    /// Bit selecting the cycle counter in `PMCNTENSET_EL0` / `PMCNTENCLR_EL0`.
    const CYCLE_COUNTER_BIT: u64 = 1 << 31;

    /// Reads `PMCCFILTR_EL0` (cycle counter filter register).
    #[inline]
    fn read_pmccfiltr_el0() -> u64 {
        let value;
        unsafe {
            asm!("mrs {}, PMCCFILTR_EL0", out(reg) value);
        }
        value
    }

    /// Writes `PMCCFILTR_EL0` (cycle counter filter register).
    #[inline]
    fn write_pmccfiltr_el0(value: u64) {
        unsafe {
            asm!("msr PMCCFILTR_EL0, {}", in(reg) value);
        }
    }

    /// Configures the cycle-counter filter, then resets the counter to 0.
    ///
    /// `PMCCFILTR_EL0.U` (bit 30) excludes EL0 counting when set, and
    /// `PMCCFILTR_EL0.P` (bit 31) excludes EL1 counting when set.
    pub fn configure(exclude_el0: bool, exclude_el1: bool) {
        let mut filter = read_pmccfiltr_el0();

        // Clear U (bit 30) and P (bit 31), then apply the requested values.
        filter &= !((1 << 30) | (1 << 31));
        if exclude_el0 {
            filter |= 1 << 30;
        }
        if exclude_el1 {
            filter |= 1 << 31;
        }
        write_pmccfiltr_el0(filter);

        reset();
    }

    /// Resets the cycle counter (`PMCCNTR_EL0 = 0`).
    pub fn reset() {
        unsafe {
            asm!("msr PMCCNTR_EL0, {}", in(reg) 0u64);
        }
    }

    /// Enables the cycle counter (`PMCNTENSET_EL0 |= 1 << 31`).
    pub fn enable() {
        unsafe {
            asm!("msr PMCNTENSET_EL0, {}", in(reg) CYCLE_COUNTER_BIT);
        }
    }

    /// Disables the cycle counter (`PMCNTENCLR_EL0 = 1 << 31`).
    pub fn disable() {
        unsafe {
            asm!("msr PMCNTENCLR_EL0, {}", in(reg) CYCLE_COUNTER_BIT);
        }
    }

    /// Reads the cycle counter (`PMCCNTR_EL0`).
    pub fn read() -> u64 {
        let value;
        unsafe {
            asm!("mrs {}, PMCCNTR_EL0", out(reg) value);
        }
        value
    }
}
