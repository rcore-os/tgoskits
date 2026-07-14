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

use super::TrapFrame;

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
    let num_counters = ((read_pmcr_el0() >> 11) & 0x1f) as usize;
    Some(PmuInfo { num_counters })
}

/// Per-CPU one-time init: set `PMCR_EL0.E` (global counter enable) and reset all
/// event counters once so they start clean.
///
/// Idempotent and safe to call on each CPU. No-op if [`probe`] returns `None`.
/// Does not touch the dedicated cycle counter (`PMCCNTR_EL0`), whose own reset is
/// controlled by `PMCR_EL0.C` and left to [`cycles`].
pub fn init_cpu() {
    if !pmu_present() {
        return;
    }

    // PMCR_EL0.E (bit 0): enable all counters.
    // PMCR_EL0.P (bit 1, W1): reset all programmable event counters to 0.
    let pmcr = read_pmcr_el0();
    write_pmcr_el0(pmcr | (1 << 0) | (1 << 1));

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

/// Which CPU cluster (microarchitecture) a core belongs to, decoded from
/// `MIDR_EL1`. RK3588 is big.LITTLE: Cortex-A76 "big" + Cortex-A55 "LITTLE".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClusterId {
    /// Cortex-A55 ("LITTLE"), `MIDR_EL1` partnum `0xD05`.
    Little,
    /// Cortex-A76 ("big"), `MIDR_EL1` partnum `0xD0B`.
    Big,
    /// Any other implementation (e.g. QEMU `virt`'s Cortex-A53 `0xD03`); the
    /// raw partnum is carried for diagnostics.
    Other(u16),
}

/// `MIDR_EL1.Implementer` value for Arm Limited.
const MIDR_IMPLEMENTER_ARM: u64 = 0x41;
/// `MIDR_EL1.PartNum` for Cortex-A55.
const MIDR_PARTNUM_CORTEX_A55: u64 = 0xD05;
/// `MIDR_EL1.PartNum` for Cortex-A76.
const MIDR_PARTNUM_CORTEX_A76: u64 = 0xD0B;

/// Classify a raw `MIDR_EL1` value into a [`ClusterId`].
///
/// Mirrors Linux's `MIDR_CPU_MODEL_MASK` comparison (implementer + partnum;
/// variant/revision excluded). Pure so it is host-unit-testable.
pub fn classify_midr(midr: u64) -> ClusterId {
    let implementer = (midr >> 24) & 0xff;
    let partnum = (midr >> 4) & 0xfff;
    if implementer != MIDR_IMPLEMENTER_ARM {
        return ClusterId::Other(partnum as u16);
    }
    match partnum {
        MIDR_PARTNUM_CORTEX_A55 => ClusterId::Little,
        MIDR_PARTNUM_CORTEX_A76 => ClusterId::Big,
        other => ClusterId::Other(other as u16),
    }
}

/// Classify the current core into a [`ClusterId`] from `MIDR_EL1`.
///
/// Must be called *on* the core being classified — `MIDR_EL1` reflects only the
/// executing PE.
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
    match event {
        0x00..=0x1F => (read_pmceid0_el0() >> event) & 1 != 0,
        0x20..=0x3F => (read_pmceid1_el0() >> (event - 0x20)) & 1 != 0,
        _ => true,
    }
}

/// Maps a Linux `perf_hw_id` to an ARMv8 PMUv3 common event number.
///
/// Mirrors the kernel's `armv8_pmuv3_perf_map`. `hw_id` is the plain numeric
/// `perf_hw_id` discriminant; this crate stays free of `kbpf`, so the mapping
/// takes a raw `u32` rather than an enum. Returns `None` for unmapped ids
/// (including `REF_CPU_CYCLES` and anything out of range).
pub fn hw_event_to_arm(hw_id: u32) -> Option<u16> {
    match hw_id {
        // PERF_COUNT_HW_CPU_CYCLES => CPU_CYCLES.
        0 => Some(0x11),
        // PERF_COUNT_HW_INSTRUCTIONS => INST_RETIRED.
        1 => Some(0x08),
        // PERF_COUNT_HW_CACHE_REFERENCES => L1D_CACHE.
        2 => Some(0x04),
        // PERF_COUNT_HW_CACHE_MISSES => L1D_CACHE_REFILL.
        3 => Some(0x03),
        // PERF_COUNT_HW_BRANCH_INSTRUCTIONS => BR_RETIRED.
        4 => Some(0x21),
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

/// Maps a `PERF_TYPE_HW_CACHE` config to an ARM PMUv3 event number, or `None`
/// for a combination ARM PMUv3 does not define (matching Linux's
/// `CACHE_OP_UNSUPPORTED`, which rejects the event at open).
///
/// The config packs `cache_id | (op << 8) | (result << 16)`: `cache_id` is
/// L1D(0)/L1I(1)/LL(2)/DTLB(3)/ITLB(4)/BPU(5)/NODE(6), `op` is READ(0)/WRITE(1)/
/// PREFETCH(2), `result` is ACCESS(0)/MISS(1). ARM's basic cache events do not
/// split read vs write, so both map to the same event; PREFETCH and NODE have no
/// architectural counterpart and are rejected.
pub fn hw_cache_to_arm(config: u64) -> Option<u16> {
    const READ: u8 = 0;
    const WRITE: u8 = 1;
    const ACCESS: u8 = 0;
    const MISS: u8 = 1;

    let cache_id = (config & 0xff) as u8;
    let op = ((config >> 8) & 0xff) as u8;
    let result = ((config >> 16) & 0xff) as u8;

    Some(match (cache_id, op, result) {
        // L1D: L1D_CACHE / L1D_CACHE_REFILL.
        (0, READ | WRITE, ACCESS) => 0x04,
        (0, READ | WRITE, MISS) => 0x03,
        // L1I: L1I_CACHE / L1I_CACHE_REFILL.
        (1, READ, ACCESS) => 0x14,
        (1, READ, MISS) => 0x01,
        // LL (last level): LL_CACHE_RD / LL_CACHE_MISS_RD for reads, else
        // LL_CACHE / LL_CACHE_MISS.
        (2, READ, ACCESS) => 0x36,
        (2, READ, MISS) => 0x37,
        (2, WRITE, ACCESS) => 0x32,
        (2, WRITE, MISS) => 0x33,
        // DTLB: L1D_TLB / L1D_TLB_REFILL.
        (3, READ | WRITE, ACCESS) => 0x25,
        (3, READ | WRITE, MISS) => 0x05,
        // ITLB: L1I_TLB / L1I_TLB_REFILL.
        (4, READ, ACCESS) => 0x26,
        (4, READ, MISS) => 0x02,
        // BPU (branch prediction): BR_PRED / BR_MIS_PRED.
        (5, READ | WRITE, ACCESS) => 0x12,
        (5, READ | WRITE, MISS) => 0x10,
        // PREFETCH ops, NODE cache, and every other combination are unsupported.
        _ => return None,
    })
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

    /// Disables every counter at once (`PMCNTENCLR_EL0 = 0xFFFF_FFFF`), the
    /// programmable counters and the cycle counter (bit 31). Used by the
    /// per-core clean-slate bring-up so a freshly-entered secondary core starts
    /// with nothing counting.
    pub fn disable_all() {
        unsafe {
            asm!("msr PMCNTENCLR_EL0, {}", in(reg) 0xFFFF_FFFFu64);
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

    /// Masks the overflow interrupt for every counter
    /// (`PMINTENCLR_EL1 = 0xFFFF_FFFF`). Used by the per-core clean-slate
    /// bring-up so a freshly-entered secondary core has no overflow IRQ armed.
    pub fn disable_all_irq() {
        unsafe {
            asm!("msr PMINTENCLR_EL1, {}", in(reg) 0xFFFF_FFFFu64);
        }
    }

    /// Clears every overflow-status flag (`PMOVSCLR_EL0 = 0xFFFF_FFFF`,
    /// write-1-to-clear). Used by the per-core clean-slate bring-up so a stale
    /// overflow flag cannot raise a spurious PMU interrupt.
    pub fn clear_all() {
        unsafe {
            asm!("msr PMOVSCLR_EL0, {}", in(reg) 0xFFFF_FFFFu64);
        }
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

/// Per-CPU pointer to the [`TrapFrame`] of the context interrupted by the IRQ
/// currently being dispatched, or `0` when not inside an IRQ dispatch.
///
/// Published at the two IRQ-entry sites (EL1 kernel interrupt, EL0 user
/// interrupt) immediately before `dispatch_irq` and cleared immediately after,
/// so the PMU overflow handler — which runs *inside* `dispatch_irq` — can read
/// the interrupted frame pointer (`x29`) for `PERF_SAMPLE_CALLCHAIN` unwinding.
/// The live `ELR_EL1`/`SPSR_EL1` (see [`interrupted_pc`]/[`interrupted_is_user`])
/// still describe the interrupted PC and EL, but `x29` survives only in the
/// saved frame — the handler's own frames have long since clobbered the GPR.
#[ax_percpu::def_percpu]
static PMU_TRAP_FRAME: usize = 0;

/// Publishes `tf` as the interrupted trap frame for the current CPU.
///
/// Called at IRQ entry, immediately before `dispatch_irq`, and paired with
/// [`clear_trap_frame`] immediately after it returns. A single per-CPU store
/// (TPIDR-relative) that cannot fault and does not touch the register
/// save/restore path.
///
/// # Safety
/// `tf` must point to a valid [`TrapFrame`] that stays alive until
/// [`clear_trap_frame`] runs on this CPU (i.e. for the duration of the IRQ
/// dispatch). Interrupts must remain masked across the publish → dispatch →
/// clear window so no nested IRQ observes this CPU's pointer for a frame that
/// has already returned.
#[inline]
pub unsafe fn set_trap_frame(tf: *const TrapFrame) {
    PMU_TRAP_FRAME.write_current(tf as usize);
}

/// Clears the published interrupted trap frame for the current CPU.
///
/// Called immediately after `dispatch_irq` returns, so no later sampling
/// interrupt can observe a stale (already-returned) frame.
#[inline]
pub fn clear_trap_frame() {
    PMU_TRAP_FRAME.write_current(0);
}

/// The interrupted frame pointer (`x29`), from the trap frame published at IRQ
/// entry.
///
/// Returns `None` when no frame is published — e.g. the overflow was taken on a
/// path that does not plumb the frame (synchronous exceptions), or before the
/// plumbing ran. Callers must fall back to a leaf-only callchain in that case
/// rather than skipping the sample.
pub fn interrupted_fp() -> Option<usize> {
    let p = PMU_TRAP_FRAME.read_current();
    if p == 0 {
        return None;
    }
    // SAFETY: `p` was published from a live `&TrapFrame` by `set_trap_frame` and
    // is only non-zero for the duration of the IRQ dispatch on this CPU, during
    // which we are running.
    let tf = p as *const TrapFrame;
    Some(unsafe { (*tf).x[29] as usize })
}

/// The interrupted stack pointer.
///
/// For a user (EL0) interrupt the interrupted SP lives in `SP_EL0`, which the
/// kernel never overwrites (it runs on `SP_EL1`), so it still holds the user SP
/// and is read live. For a kernel (EL1) interrupt it is the pre-trap SP saved in
/// the published frame. Returns `None` if no frame is published on the kernel
/// path. Used only to bound the user-stack unwind window.
pub fn interrupted_sp() -> Option<usize> {
    if interrupted_is_user() {
        let sp: u64;
        unsafe {
            asm!("mrs {}, SP_EL0", out(reg) sp);
        }
        return Some(sp as usize);
    }
    let p = PMU_TRAP_FRAME.read_current();
    if p == 0 {
        return None;
    }
    // SAFETY: see `interrupted_fp`.
    let tf = p as *const TrapFrame;
    Some(unsafe { (*tf).sp as usize })
}

#[cfg(test)]
mod hw_cache_tests {
    use super::hw_cache_to_arm;

    /// Pack a `PERF_TYPE_HW_CACHE` config: `cache_id | (op << 8) | (result << 16)`,
    /// exactly as `perf` encodes it.
    fn cfg(cache_id: u64, op: u64, result: u64) -> u64 {
        cache_id | (op << 8) | (result << 16)
    }

    // Linux `perf_hw_cache_id` / `_op_id` / `_op_result_id` values.
    const L1D: u64 = 0;
    const L1I: u64 = 1;
    const LL: u64 = 2;
    const DTLB: u64 = 3;
    const ITLB: u64 = 4;
    const BPU: u64 = 5;
    const NODE: u64 = 6;
    const READ: u64 = 0;
    const WRITE: u64 = 1;
    const PREFETCH: u64 = 2;
    const ACCESS: u64 = 0;
    const MISS: u64 = 1;

    /// Every supported (cache, op, result) triple maps to its exact ARM PMUv3
    /// event number. This is the deterministic regression guard for the mapping:
    /// unlike the QEMU `perf-hw-cache` case (QEMU-TCG implements no cache events,
    /// so it can only check open-or-unsupported), this pins the config→event decode
    /// itself, independent of any CPU's implemented event set.
    #[test]
    fn supported_combos_map_to_expected_arm_events() {
        let cases: &[(u64, u64, u64, u16)] = &[
            // L1D_CACHE (0x04) / L1D_CACHE_REFILL (0x03), read and write.
            (L1D, READ, ACCESS, 0x04),
            (L1D, WRITE, ACCESS, 0x04),
            (L1D, READ, MISS, 0x03),
            (L1D, WRITE, MISS, 0x03),
            // L1I_CACHE (0x14) / L1I_CACHE_REFILL (0x01), read only.
            (L1I, READ, ACCESS, 0x14),
            (L1I, READ, MISS, 0x01),
            // LL cache, read vs write, access vs miss.
            (LL, READ, ACCESS, 0x36),
            (LL, READ, MISS, 0x37),
            (LL, WRITE, ACCESS, 0x32),
            (LL, WRITE, MISS, 0x33),
            // L1D_TLB (0x25) / L1D_TLB_REFILL (0x05), read and write.
            (DTLB, READ, ACCESS, 0x25),
            (DTLB, WRITE, ACCESS, 0x25),
            (DTLB, READ, MISS, 0x05),
            (DTLB, WRITE, MISS, 0x05),
            // L1I_TLB (0x26) / L1I_TLB_REFILL (0x02), read only.
            (ITLB, READ, ACCESS, 0x26),
            (ITLB, READ, MISS, 0x02),
            // BR_PRED (0x12) / BR_MIS_PRED (0x10), read and write.
            (BPU, READ, ACCESS, 0x12),
            (BPU, WRITE, ACCESS, 0x12),
            (BPU, READ, MISS, 0x10),
            (BPU, WRITE, MISS, 0x10),
        ];
        for &(c, o, r, ev) in cases {
            assert_eq!(
                hw_cache_to_arm(cfg(c, o, r)),
                Some(ev),
                "cache={c} op={o} result={r} should map to {ev:#04x}"
            );
        }
    }

    /// PREFETCH has no ARM PMUv3 counterpart for any cache: always unsupported
    /// (Linux rejects it with `CACHE_OP_UNSUPPORTED` too).
    #[test]
    fn prefetch_ops_are_unsupported() {
        for cache in [L1D, L1I, LL, DTLB, ITLB, BPU] {
            for result in [ACCESS, MISS] {
                assert_eq!(
                    hw_cache_to_arm(cfg(cache, PREFETCH, result)),
                    None,
                    "prefetch (cache={cache}) must be unsupported"
                );
            }
        }
    }

    /// The NODE cache class has no ARM PMUv3 mapping: unsupported for every op.
    #[test]
    fn node_cache_is_unsupported() {
        for op in [READ, WRITE, PREFETCH] {
            for result in [ACCESS, MISS] {
                assert_eq!(hw_cache_to_arm(cfg(NODE, op, result)), None);
            }
        }
    }

    /// The instruction side (L1I / ITLB) has no write counters, so a write op is
    /// unsupported — guards against accidentally widening those arms to `WRITE`.
    #[test]
    fn instruction_side_write_is_unsupported() {
        assert_eq!(hw_cache_to_arm(cfg(L1I, WRITE, ACCESS)), None);
        assert_eq!(hw_cache_to_arm(cfg(L1I, WRITE, MISS)), None);
        assert_eq!(hw_cache_to_arm(cfg(ITLB, WRITE, ACCESS)), None);
        assert_eq!(hw_cache_to_arm(cfg(ITLB, WRITE, MISS)), None);
    }

    /// Out-of-range cache/op/result ids decode to no event (not a panic, not a
    /// wrong event).
    #[test]
    fn invalid_ids_are_unsupported() {
        assert_eq!(hw_cache_to_arm(cfg(7, READ, ACCESS)), None); // no such cache id
        assert_eq!(hw_cache_to_arm(cfg(L1D, READ, 2)), None); // no such result id
        assert_eq!(hw_cache_to_arm(cfg(L1D, 3, ACCESS)), None); // no such op id
    }
}
