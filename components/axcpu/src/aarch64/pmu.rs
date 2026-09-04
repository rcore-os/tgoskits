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

/// Information probed from the current CPU's PMU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PmuInfo {
    /// `PMCR_EL0.N`: number of programmable event counters.
    ///
    /// The dedicated cycle counter (`PMCCNTR_EL0`) is separate and not included
    /// in this count.
    pub num_counters: usize,
    /// Width of each programmable event counter.
    pub counter_width: u8,
    /// Width of the dedicated cycle counter.
    pub cycle_counter_width: u8,
    /// Raw `MIDR_EL1`, used to classify heterogeneous CPU clusters.
    pub midr: u64,
    /// Common-event capability bitmap for event ids `0x00..=0x1f`.
    pub pmceid0: u32,
    /// Common-event capability bitmap for event ids `0x20..=0x3f`.
    pub pmceid1: u32,
}

impl PmuInfo {
    /// Returns whether this PMU implements an architectural common event.
    pub const fn event_supported(self, event: u16) -> bool {
        match event {
            0x00..=0x1f => self.pmceid0 & (1 << event) != 0,
            0x20..=0x3f => self.pmceid1 & (1 << (event - 0x20)) != 0,
            _ => true,
        }
    }
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

/// Writes `PMUSERENR_EL0` (user-mode enable register), which gates EL0 access to
/// the PMU registers.
#[inline]
fn write_pmuserenr_el0(value: u64) {
    unsafe {
        asm!("msr PMUSERENR_EL0, {}", in(reg) value);
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
    let pmcr = read_pmcr_el0();
    let num_counters = ((pmcr >> 11) & 0x1f) as usize;
    Some(PmuInfo {
        num_counters,
        // Starry does not enable PMUv3.5 long programmable counters yet.
        counter_width: 32,
        cycle_counter_width: if pmcr & (1 << 6) != 0 { 64 } else { 32 },
        midr: read_midr_el1(),
        pmceid0: read_pmceid0_el0() as u32,
        pmceid1: read_pmceid1_el0() as u32,
    })
}

/// Per-CPU one-time init: enable long cycle counting, set `PMCR_EL0.E` (global
/// counter enable), and reset all event counters once so they start clean.
///
/// Idempotent and safe to call on each CPU. No-op if [`probe`] returns `None`.
pub fn init_cpu() {
    if !pmu_present() {
        return;
    }

    // PMCR_EL0.E (bit 0): enable all counters.
    // PMCR_EL0.P (bit 1, W1): reset all programmable event counters to 0.
    // PMCR_EL0.C (bit 2, W1): reset the dedicated cycle counter to 0.
    // PMCR_EL0.LC (bit 6): use the architectural 64-bit cycle counter. This is
    // the same reset policy as Linux armv8pmu_reset(); it must run only once per
    // CPU because P/C are destructive to already-running events.
    let pmcr = read_pmcr_el0();
    write_pmcr_el0(pmcr | (1 << 0) | (1 << 1) | (1 << 2) | (1 << 6));

    // Allow EL0 to read the counters directly, for `rdpmc`-style self-monitoring
    // (a process reads its event via `mrs PMEVCNTRn_EL0` / `PMCCNTR_EL0` using
    // the `perf_event_mmap_page` it mapped, with no syscall):
    //   PMUSERENR_EL0.ER (bit 3) = EL0 read of the event counters + `PMSELR_EL0`,
    //   PMUSERENR_EL0.CR (bit 2) = EL0 read of the cycle counter `PMCCNTR_EL0`.
    // EN (bit 0, full unprivileged access) and SW (software increment) are left
    // clear — read access only. Matches the unrestricted `perf_event_paranoid`
    // (`-1`) this kernel advertises in `/proc/sys/kernel`.
    write_pmuserenr_el0((1 << 3) | (1 << 2));
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

/// CPU-cluster class derived from `MIDR_EL1`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClusterId {
    /// Arm Cortex-A55, used by the RK3588 LITTLE cluster.
    CortexA55,
    /// Arm Cortex-A76, used by the RK3588 big clusters.
    CortexA76,
    /// Any other CPU, retaining its implementer and part number.
    Other {
        /// `MIDR_EL1.Implementer`.
        implementer: u8,
        /// `MIDR_EL1.PartNum`.
        part: u16,
    },
}

/// Classifies a raw `MIDR_EL1` using the same implementer/part fields as Linux.
pub const fn classify_midr(midr: u64) -> ClusterId {
    let implementer = ((midr >> 24) & 0xff) as u8;
    let part = ((midr >> 4) & 0xfff) as u16;
    match (implementer, part) {
        (0x41, 0xd05) => ClusterId::CortexA55,
        (0x41, 0xd0b) => ClusterId::CortexA76,
        _ => ClusterId::Other { implementer, part },
    }
}

/// Returns the current CPU's cluster class.
pub fn cluster_id() -> ClusterId {
    classify_midr(read_midr_el1())
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

/// Reads `PMCEID0_EL0` (common event identification register 0).
///
/// Bit `e` (for `e` in `0x00..=0x1F`) reads as 1 iff common event `e` is
/// implemented.
#[inline]
fn read_pmceid0_el0() -> u64 {
    let value;
    unsafe {
        asm!("mrs {}, PMCEID0_EL0", out(reg) value);
    }
    value
}

/// Reads `PMCEID1_EL0` (common event identification register 1).
///
/// Bit `e - 0x20` (for `e` in `0x20..=0x3F`) reads as 1 iff common event `e` is
/// implemented.
#[inline]
fn read_pmceid1_el0() -> u64 {
    let value;
    unsafe {
        asm!("mrs {}, PMCEID1_EL0", out(reg) value);
    }
    value
}

/// Returns whether ARM `event` is architecturally supported on this CPU.
///
/// `PMCEID0_EL0` covers common events `0x00..=0x1F` and `PMCEID1_EL0` covers
/// `0x20..=0x3F`, each as a bitmap. Events `>= 0x40` are IMPLEMENTATION DEFINED
/// or otherwise outside the common-event bitmaps and cannot be validated here,
/// so they are let through (return `true`).
pub fn event_supported(event: u16) -> bool {
    probe().is_some_and(|info| info.event_supported(event))
}

/// Maps a Linux `perf_hw_id` to an ARMv8 PMUv3 common event number.
///
/// Mirrors the kernel's `armv8_pmuv3_perf_map`. `hw_id` is the plain numeric
/// `perf_hw_id` discriminant; this crate stays free of `kbpf`, so the mapping
/// takes a raw `u32` rather than an enum. Returns `None` for unmapped ids
/// (including `REF_CPU_CYCLES` and anything out of range).
pub fn hw_event_to_arm(hw_id: u32) -> Option<u16> {
    let info = probe()?;
    hw_event_to_arm_with(info, hw_id)
}

/// Maps a Linux generic hardware event using one CPU's cached PMCEID bitmap.
pub const fn hw_event_to_arm_with(info: PmuInfo, hw_id: u32) -> Option<u16> {
    match hw_id {
        // PERF_COUNT_HW_CPU_CYCLES => CPU_CYCLES.
        0 => Some(0x11),
        // PERF_COUNT_HW_INSTRUCTIONS => INST_RETIRED.
        1 => Some(0x08),
        // PERF_COUNT_HW_CACHE_REFERENCES => L1D_CACHE.
        2 => Some(0x04),
        // PERF_COUNT_HW_CACHE_MISSES => L1D_CACHE_REFILL.
        3 => Some(0x03),
        // Linux prefers BR_RETIRED and falls back to PC_WRITE_RETIRED.
        4 if info.event_supported(0x21) => Some(0x21),
        4 if info.event_supported(0x0c) => Some(0x0c),
        4 => None,
        // PERF_COUNT_HW_BRANCH_MISSES => BR_MIS_PRED.
        5 => Some(0x10),
        // PERF_COUNT_HW_BUS_CYCLES => BUS_CYCLES.
        6 => Some(0x1D),
        // PERF_COUNT_HW_STALLED_CYCLES_FRONTEND => STALL_FRONTEND.
        7 => Some(0x23),
        // PERF_COUNT_HW_STALLED_CYCLES_BACKEND => STALL_BACKEND.
        8 => Some(0x24),
        // PERF_COUNT_HW_REF_CPU_CYCLES (9) and anything else are unmapped.
        _ => None,
    }
}

/// Failure class returned by [`hw_cache_to_arm`].
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheEventError {
    /// A cache, operation, or result selector is outside the Linux UAPI range.
    Invalid,
    /// The selector is valid but the generic ARM PMUv3 map has no event for it.
    Unsupported,
}

/// Maps Linux `PERF_TYPE_HW_CACHE` onto the generic ARM PMUv3 event table.
///
/// Cortex-A55 and Cortex-A76 use Linux's `PMUV3_INIT_SIMPLE` setup, so only
/// generic read operations are accepted. Microarchitecture-specific A53 maps
/// are intentionally not applied to A55/A76.
pub const fn hw_cache_to_arm(config: u64) -> Result<u16, CacheEventError> {
    let cache = (config & 0xff) as u8;
    let operation = ((config >> 8) & 0xff) as u8;
    let result = ((config >> 16) & 0xff) as u8;
    if cache >= 7 || operation >= 3 || result >= 2 {
        return Err(CacheEventError::Invalid);
    }
    if operation != 0 {
        return Err(CacheEventError::Unsupported);
    }
    match (cache, result) {
        (0, 0) => Ok(0x04), // L1D access
        (0, 1) => Ok(0x03), // L1D refill
        (1, 0) => Ok(0x14), // L1I access
        (1, 1) => Ok(0x01), // L1I refill
        (2, 0) => Ok(0x36), // last-level read
        (2, 1) => Ok(0x37), // last-level read miss
        (3, 0) => Ok(0x25), // DTLB access
        (3, 1) => Ok(0x05), // DTLB refill
        (4, 0) => Ok(0x26), // ITLB access
        (4, 1) => Ok(0x02), // ITLB refill
        (5, 0) => Ok(0x12), // branch prediction access
        (5, 1) => Ok(0x10), // branch misprediction
        _ => Err(CacheEventError::Unsupported),
    }
}

/// The generic programmable event counters (`PMEVCNTRn_EL0` / `PMEVTYPERn_EL0`).
///
/// `n` is the logical counter index in `0..num_counters` (from
/// [`PmuInfo::num_counters`]). Counters are 32-bit on this layer (no chaining);
/// [`read`] zero-extends to `u64`.
///
/// Each counter is a distinct named system register, so accesses fan out on `n`
/// to a direct `mrs`/`msr` rather than going through `PMSELR_EL0`. Selecting via
/// `PMSELR_EL0` would be a select-then-access pair that races with any future IRQ
/// handler touching the same indirection; the named-register form is atomic per
/// access. This mirrors Linux's `PMEVN_SWITCH`.
pub mod counter {
    use core::arch::asm;

    /// Highest supported logical counter index. ARMv8 names `PMEVCNTR0_EL0`
    /// through `PMEVCNTR30_EL0` (31 programmable counters max).
    const MAX_COUNTER: usize = 30;

    /// `PMEVTYPERn_EL0.P` (bit 31): exclude EL1 from counting when set.
    const EVTYPER_P_EXCLUDE_EL1: u64 = 1 << 31;
    /// `PMEVTYPERn_EL0.U` (bit 30): exclude EL0 from counting when set.
    const EVTYPER_U_EXCLUDE_EL0: u64 = 1 << 30;
    /// `PMEVTYPERn_EL0.EVENT` mask (bits `[15:0]`).
    const EVTYPER_EVENT_MASK: u64 = 0xFFFF;

    /// Fans out on a runtime counter index `$n` to a direct `mrs`/`msr` on the
    /// named system register `<$reg><n>_EL0`.
    ///
    /// Mirrors Linux's `PMEVN_SWITCH`: the register name encodes the index, so a
    /// `match` over `0..=30` is the only way to turn a runtime `n` into a direct
    /// (race-free) register access. Two shapes:
    ///
    /// * `read` — emits `mrs {out}, <reg>` per arm and yields a `u64`; an
    ///   out-of-range `n` yields `0`.
    /// * `write` — emits `msr <reg>, {in}` per arm with the supplied value; an
    ///   out-of-range `n` is a no-op.
    macro_rules! pmev_switch {
        // Read shape: yields the named register's value, 0 if out of range.
        (read $n:expr, $reg:literal) => {{
            macro_rules! arm {
                        ($idx:literal) => {{
                            let value: u64;
                            unsafe {
                                asm!(concat!("mrs {}, ", $reg, $idx, "_EL0"), out(reg) value);
                            }
                            value
                        }};
                    }
            match $n {
                0 => arm!("0"),
                1 => arm!("1"),
                2 => arm!("2"),
                3 => arm!("3"),
                4 => arm!("4"),
                5 => arm!("5"),
                6 => arm!("6"),
                7 => arm!("7"),
                8 => arm!("8"),
                9 => arm!("9"),
                10 => arm!("10"),
                11 => arm!("11"),
                12 => arm!("12"),
                13 => arm!("13"),
                14 => arm!("14"),
                15 => arm!("15"),
                16 => arm!("16"),
                17 => arm!("17"),
                18 => arm!("18"),
                19 => arm!("19"),
                20 => arm!("20"),
                21 => arm!("21"),
                22 => arm!("22"),
                23 => arm!("23"),
                24 => arm!("24"),
                25 => arm!("25"),
                26 => arm!("26"),
                27 => arm!("27"),
                28 => arm!("28"),
                29 => arm!("29"),
                30 => arm!("30"),
                _ => 0u64,
            }
        }};
        // Write shape: writes `$value` to the named register, no-op if out of range.
        (write $n:expr, $reg:literal, $value:expr) => {{
            let v: u64 = $value;
            macro_rules! arm {
                        ($idx:literal) => {{
                            unsafe {
                                asm!(concat!("msr ", $reg, $idx, "_EL0, {}"), in(reg) v);
                            }
                        }};
                    }
            match $n {
                0 => arm!("0"),
                1 => arm!("1"),
                2 => arm!("2"),
                3 => arm!("3"),
                4 => arm!("4"),
                5 => arm!("5"),
                6 => arm!("6"),
                7 => arm!("7"),
                8 => arm!("8"),
                9 => arm!("9"),
                10 => arm!("10"),
                11 => arm!("11"),
                12 => arm!("12"),
                13 => arm!("13"),
                14 => arm!("14"),
                15 => arm!("15"),
                16 => arm!("16"),
                17 => arm!("17"),
                18 => arm!("18"),
                19 => arm!("19"),
                20 => arm!("20"),
                21 => arm!("21"),
                22 => arm!("22"),
                23 => arm!("23"),
                24 => arm!("24"),
                25 => arm!("25"),
                26 => arm!("26"),
                27 => arm!("27"),
                28 => arm!("28"),
                29 => arm!("29"),
                30 => arm!("30"),
                _ => {}
            }
        }};
    }

    /// Programs counter `n` to count ARM `event` (`PMEVTYPERn_EL0.EVENT`,
    /// bits `[15:0]`) with EL filtering, then resets the counter to 0.
    ///
    /// `exclude_el0` sets `U` (bit 30) and `exclude_el1` sets `P` (bit 31). Does
    /// NOT enable the counter; call [`enable`] separately. Out-of-range `n` is a
    /// no-op (debug builds assert).
    pub fn configure(n: usize, event: u16, exclude_el0: bool, exclude_el1: bool) {
        debug_assert!(n <= MAX_COUNTER);

        let mut evtyper = read_typer(n);
        // Clear EVENT, U and P, then apply the requested configuration.
        evtyper &= !(EVTYPER_EVENT_MASK | EVTYPER_U_EXCLUDE_EL0 | EVTYPER_P_EXCLUDE_EL1);
        evtyper |= (event as u64) & EVTYPER_EVENT_MASK;
        if exclude_el0 {
            evtyper |= EVTYPER_U_EXCLUDE_EL0;
        }
        if exclude_el1 {
            evtyper |= EVTYPER_P_EXCLUDE_EL1;
        }
        write_typer(n, evtyper);

        reset(n);
    }

    /// Enables counter `n` (`PMCNTENSET_EL0 |= 1 << n`).
    ///
    /// Out-of-range `n` is a no-op (debug builds assert).
    pub fn enable(n: usize) {
        debug_assert!(n <= MAX_COUNTER);
        if n > MAX_COUNTER {
            return;
        }
        unsafe {
            asm!("msr PMCNTENSET_EL0, {}", in(reg) 1u64 << n);
        }
    }

    /// Disables counter `n` (`PMCNTENCLR_EL0 = 1 << n`).
    ///
    /// Out-of-range `n` is a no-op (debug builds assert).
    pub fn disable(n: usize) {
        debug_assert!(n <= MAX_COUNTER);
        if n > MAX_COUNTER {
            return;
        }
        unsafe {
            asm!("msr PMCNTENCLR_EL0, {}", in(reg) 1u64 << n);
        }
    }

    /// Disables all programmable counters and the dedicated cycle counter.
    pub fn disable_all() {
        unsafe {
            asm!("msr PMCNTENCLR_EL0, {}", in(reg) u32::MAX as u64);
        }
    }

    /// Resets counter `n` (`PMEVCNTRn_EL0 = 0`).
    ///
    /// Out-of-range `n` is a no-op (debug builds assert).
    pub fn reset(n: usize) {
        write(n, 0);
    }

    /// Reads counter `n` (`PMEVCNTRn_EL0`), zero-extended from 32 bits to `u64`.
    ///
    /// Out-of-range `n` returns 0 (debug builds assert).
    pub fn read(n: usize) -> u64 {
        debug_assert!(n <= MAX_COUNTER);
        // PMEVCNTRn_EL0 is a 32-bit counter; mask defensively in case the read
        // upper bits are not architecturally zero.
        pmev_switch!(read n, "PMEVCNTR") & 0xFFFF_FFFF
    }

    /// Writes `value` to counter `n` (`PMEVCNTRn_EL0`).
    ///
    /// Only the low 32 bits are significant (32-bit counters). Used to preload a
    /// sampling period later. Out-of-range `n` is a no-op (debug builds assert).
    pub fn write(n: usize, value: u64) {
        debug_assert!(n <= MAX_COUNTER);
        pmev_switch!(write n, "PMEVCNTR", value);
    }

    /// Preloads counter `n` so it overflows after `period` events.
    ///
    /// Writes `PMEVCNTRn_EL0 = (0u32).wrapping_sub(period)`: a 32-bit counter set
    /// `period` short of wrapping past `0xFFFF_FFFF` raises its overflow (and the
    /// `PMOVSCLR_EL0` / `PMINTENSET_EL1` interrupt, if enabled) once it has counted
    /// `period` more events. The sampling IRQ handler calls this to re-arm the next
    /// sample. Out-of-range `n` is a no-op (debug builds assert).
    pub fn preload(n: usize, period: u32) {
        write(n, (0u32).wrapping_sub(period) as u64);
    }

    /// Reads `PMEVTYPERn_EL0`. Out-of-range `n` returns 0.
    fn read_typer(n: usize) -> u64 {
        pmev_switch!(read n, "PMEVTYPER")
    }

    /// Writes `PMEVTYPERn_EL0`. Out-of-range `n` is a no-op.
    fn write_typer(n: usize, value: u64) {
        pmev_switch!(write n, "PMEVTYPER", value);
    }
}

/// The PMU overflow-interrupt control registers (`PMOVSCLR_EL0`,
/// `PMINTENSET_EL1` / `PMINTENCLR_EL1`).
///
/// These drive the sampling IRQ path: a counter that wraps past its 32-bit
/// maximum sets its bit in `PMOVSCLR_EL0`, and — if armed in `PMINTENSET_EL1` —
/// asserts the PMU overflow interrupt. The handler reads [`status`] to find which
/// counters fired, services them, and [`clear`]s their bits (write-1-to-clear).
///
/// `n` is a programmable counter index in `0..=30` (matching
/// [`counter`]); bit 31 of `PMOVSCLR_EL0` is the dedicated cycle counter, which
/// M2 sampling does not use. Out-of-range `n` (`>= 32`) is guarded as a no-op.
pub mod overflow {
    use core::arch::asm;

    /// Highest programmable-counter index whose overflow bit fits below the
    /// cycle-counter bit (31). Indices `0..=30` map to bit `1 << n`.
    const MAX_COUNTER: usize = 30;

    /// Reads `PMOVSCLR_EL0` (overflow flag status): bit `n` set ⇒ programmable
    /// counter `n` overflowed; bit 31 ⇒ the cycle counter overflowed.
    ///
    /// Returns the low 32 bits, the architecturally defined extent of the flags.
    pub fn status() -> u32 {
        let value: u64;
        unsafe {
            asm!("mrs {}, PMOVSCLR_EL0", out(reg) value);
        }
        value as u32
    }

    /// Clears the given overflow-status bits (`PMOVSCLR_EL0 = mask`,
    /// write-1-to-clear).
    ///
    /// Only the bits set in `mask` are affected; writing 0 to a bit leaves it
    /// unchanged.
    pub fn clear(mask: u32) {
        unsafe {
            asm!("msr PMOVSCLR_EL0, {}", in(reg) mask as u64);
        }
    }

    /// Enables the overflow interrupt for programmable counter `n`
    /// (`PMINTENSET_EL1 |= 1 << n`).
    ///
    /// Out-of-range `n` is a no-op (debug builds assert).
    pub fn enable_irq(n: usize) {
        debug_assert!(n <= MAX_COUNTER);
        if n > MAX_COUNTER {
            return;
        }
        unsafe {
            asm!("msr PMINTENSET_EL1, {}", in(reg) 1u64 << n);
        }
    }

    /// Disables the overflow interrupt for programmable counter `n`
    /// (`PMINTENCLR_EL1 = 1 << n`).
    ///
    /// Out-of-range `n` is a no-op (debug builds assert).
    pub fn disable_irq(n: usize) {
        debug_assert!(n <= MAX_COUNTER);
        if n > MAX_COUNTER {
            return;
        }
        unsafe {
            asm!("msr PMINTENCLR_EL1, {}", in(reg) 1u64 << n);
        }
    }

    /// Disables overflow interrupts for every PMU counter on this CPU.
    pub fn disable_all_irq() {
        unsafe {
            asm!("msr PMINTENCLR_EL1, {}", in(reg) u32::MAX as u64);
        }
    }

    /// Clears all pending PMU overflow flags on this CPU.
    pub fn clear_all() {
        clear(u32::MAX);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const fn info(pmceid0: u32, pmceid1: u32) -> PmuInfo {
        PmuInfo {
            num_counters: 6,
            counter_width: 32,
            cycle_counter_width: 64,
            midr: 0,
            pmceid0,
            pmceid1,
        }
    }

    #[test]
    fn classifies_rk3588_clusters_by_midr_part() {
        assert_eq!(classify_midr(0x410f_d050), ClusterId::CortexA55);
        assert_eq!(classify_midr(0x410f_d0b0), ClusterId::CortexA76);
        assert_eq!(
            classify_midr(0x410f_d030),
            ClusterId::Other {
                implementer: 0x41,
                part: 0xd03
            }
        );
    }

    #[test]
    fn branch_mapping_uses_linux_pmceid_fallback_order() {
        assert_eq!(hw_event_to_arm_with(info(1 << 12, 1 << 1), 4), Some(0x21));
        assert_eq!(hw_event_to_arm_with(info(1 << 12, 0), 4), Some(0x0c));
        assert_eq!(hw_event_to_arm_with(info(0, 0), 4), None);
    }

    #[test]
    fn generic_cache_map_accepts_read_and_rejects_write() {
        assert_eq!(hw_cache_to_arm(0), Ok(0x04));
        assert_eq!(hw_cache_to_arm(1 << 16), Ok(0x03));
        assert_eq!(hw_cache_to_arm(1 << 8), Err(CacheEventError::Unsupported));
        assert_eq!(hw_cache_to_arm(7), Err(CacheEventError::Invalid));
    }
}

/// The interrupted program counter (`ELR_EL1`).
///
/// Read at the top of the PMU overflow IRQ handler, this is the PC the CPU was
/// executing when the sampling interrupt was taken — the value reported by
/// `PERF_SAMPLE_IP`.
pub fn interrupted_pc() -> u64 {
    let value;
    unsafe {
        asm!("mrs {}, ELR_EL1", out(reg) value);
    }
    value
}

/// Whether the interrupted context was EL0 (user).
///
/// Reads `SPSR_EL1.M[3:0]`: the value `0b0000` is `EL0t`, so the sample landed in
/// user mode iff the low four bits are zero. Any other mode (`EL1t` / `EL1h` /
/// AArch32 modes) is kernel/non-EL0.
pub fn interrupted_is_user() -> bool {
    let spsr: u64;
    unsafe {
        asm!("mrs {}, SPSR_EL1", out(reg) spsr);
    }
    (spsr & 0xf) == 0
}
