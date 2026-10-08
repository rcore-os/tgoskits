//! AArch64 System Counter, PMU and CPU-id access for the task-switch guest.
//!
//! The ArceOS standard-library benchmark is not a user process: its tasks run
//! at EL1, so they read `PMCCNTR_EL0`, `CNTVCT_EL0` and `CNTFRQ_EL0` at EL1.
//!
//! Module-level board premise assumed by every `SAFETY` note below: the EL1
//! guest cannot program the EL2 PMU trap controls, so the PMU registers are only
//! reachable when the board boot environment leaves the MDCR_EL2 `TPM` and
//! `TPMCR` traps open. This is a deployment precondition of this case, not an
//! AxVisor source guarantee:
//! `virtualization/axvm/src/arch/aarch64/policy/vcpu.rs::init_vm_context`
//! programs HCR_EL2 with only VM, TSC, TWI, RW, IMO and FMO, none of which
//! relate to PMU trapping, and no MDCR_EL2 write exists in the repository. If a
//! PMU trap is armed at runtime, the guest cannot continue measuring and the
//! board failure gate rejects the run instead of accepting a reading.
//!
//! `PMUSERENR_EL0` opens the EL0 read path (`enable_cpu_cycle` retains that
//! authorization from the original Rust-Shyper sequence); it is not the source
//! of the benchmark's own EL1 permission.
//!
//! The benchmark's single main task is the only writer of the PMU control
//! registers here: it runs the `enable_*`/`reset_*` helpers before the switch
//! workload, while the peer task only toggles GPIO3_C6. Both switch tasks run on
//! the one vCPU the VM config pins to a single physical CPU (`cpu_num = 1`,
//! `phys_cpu_ids = [0x400]`), so this benchmark's PMU state has one serial owner.

/// Reads the guest `MPIDR_EL1`, the virtual affinity from `VMPIDR_EL2`.
///
/// `virtualization/axvm/src/arch/aarch64/policy/vcpu.rs` sets `VMPIDR_EL2`
/// (`system.vmpidr_el2`), so an EL1 read here returns the virtual CPU affinity
/// AxVisor assigned, not the physical PE's own `MPIDR_EL1`.
pub fn read_mpidr() -> u64 {
    let reg_r: u64;
    // SAFETY: `mrs` from `MPIDR_EL1` is a read-only 64-bit system-register
    // access. The task runs at EL1, so it returns the virtual affinity from
    // VMPIDR_EL2 and cannot fault. `out(reg)` writes the full 64-bit result into
    // an uninitialized local, and reading a system register has no memory or
    // compiler-visible side effect, so no barrier is needed.
    unsafe {
        core::arch::asm!("mrs {}, MPIDR_EL1", out(reg) reg_r);
    }
    reg_r
}

/// Extracts the cluster id from a guest `MPIDR_EL1` virtual affinity.
///
/// The value is the `VMPIDR_EL2` affinity, which keeps the RK3588 cluster in
/// bits[15:8] and core in bits[7:0]. The benchmark prints the cluster only to
/// show which affinity AxVisor assigned, not to assert the physical PE.
pub fn mpidr2cpuid(mpidr: u64) -> usize {
    ((mpidr >> 8) & 0xff) as usize
}

/// Returns the cluster id of the current vCPU's virtual affinity.
pub fn get_cpu_id() -> usize {
    mpidr2cpuid(read_mpidr())
}

/// Instruction Synchronization Barrier.
pub fn isb() {
    // SAFETY: `isb` is the architecturally defined instruction barrier that
    // flushes the pipeline so later instructions observe prior system-register
    // writes. It touches no memory and is unconditional, so it cannot fault.
    unsafe {
        core::arch::asm!("isb");
    }
}

/// Reads the System Counter (`CNTVCT_EL0`) in ticks.
pub fn timer_cnt() -> u64 {
    let cnt: u64;
    // SAFETY: `CNTVCT_EL0` is a read-only 64-bit virtual counter readable by
    // this EL1 guest; the access cannot fault and has no compiler-visible side
    // effect. `out(reg)` fills the full 64-bit local.
    unsafe { core::arch::asm!("mrs {0}, cntvct_el0", out(reg) cnt) };
    cnt
}

/// Reads the System Counter frequency (`CNTFRQ_EL0`) in Hz.
///
/// The register is 64-bit wide (an access of 64 bits), but only bits[31:0] carry
/// the valid frequency, so the reported value always fits the low half.
pub fn timer_freq() -> u64 {
    let freq: u64;
    // SAFETY: `CNTFRQ_EL0` is a read-only 64-bit register readable by this EL1
    // guest; reading it cannot fault and needs no barrier. Only bits[31:0] are
    // meaningful, and the zero-extended read still returns the full 64-bit
    // width.
    unsafe { core::arch::asm!("mrs {0}, cntfrq_el0", out(reg) freq) };
    freq
}

/// Reads `PMUSERENR_EL0` for the bring-up log.
pub fn armv8_pmuserenr() -> u64 {
    let value: u64;
    // SAFETY: `PMUSERENR_EL0` is a readable 64-bit PMU control register. Under
    // the module-level board PMU premise it is reachable at EL1, so this is a
    // plain aligned register load with no memory aliasing and no
    // compiler-visible side effect.
    unsafe { core::arch::asm!("mrs {0}, pmuserenr_el0", out(reg) value) };
    value
}

/// Reads `PMCNTENSET_EL0` for the bring-up log.
pub fn armv8_pmcntenset() -> u64 {
    let value: u64;
    // SAFETY: `PMCNTENSET_EL0` is a readable 64-bit counter-enable register,
    // reachable at EL1 under the module-level board PMU premise. It reads a
    // control register rather than Rust memory, so there is no aliasing
    // obligation and no barrier is needed for a single dependent read.
    unsafe { core::arch::asm!("mrs {0}, pmcntenset_el0", out(reg) value) };
    value
}

/// Reads `PMCR_EL0` for the bring-up log.
pub fn armv8_pmcr() -> u64 {
    let value: u64;
    // SAFETY: `PMCR_EL0` is a readable 64-bit PMU control register, reachable at
    // EL1 under the module-level board PMU premise. It loads a system register
    // rather than Rust memory, so there is no aliasing or ordering obligation
    // for this read.
    unsafe { core::arch::asm!("mrs {0}, pmcr_el0", out(reg) value) };
    value
}

const ARMV8_PMCR_E: u64 = 1 << 0; /* Enable all counters */
const ARMV8_PMCR_P: u64 = 1 << 1; /* Reset all counters */
const ARMV8_PMCR_C: u64 = 1 << 2; /* Reset the cycle counter */
const ARMV8_PMCR_D: u64 = 1 << 3; /* CCNT counts every 64th cycle */
const ARMV8_PMCR_X: u64 = 1 << 4; /* Export events to ETM */
const ARMV8_PMCR_DP: u64 = 1 << 5; /* Disable CCNT when non-invasive debug is off */
const ARMV8_PMCR_LC: u64 = 1 << 6; /* 64-bit cycle counter (no 32-bit wrap) */

/// Writable `PMCR_EL0` bits the benchmark programs.
///
/// `LC` (bit 6) must stay inside the mask: `armv8_pmcr_set` masks its argument
/// with this value, so dropping bit 6 would clear the 64-bit cycle-counter
/// configuration and let `PMCCNTR_EL0` wrap at 32 bits.
const ARMV8_PMCR_MASK: u64 = ARMV8_PMCR_E
    | ARMV8_PMCR_P
    | ARMV8_PMCR_C
    | ARMV8_PMCR_D
    | ARMV8_PMCR_X
    | ARMV8_PMCR_DP
    | ARMV8_PMCR_LC;

/// Value written to `PMUSERENR_EL0` to grant EL0 PMU read access.
///
/// Sets EN(bit 0), SW(bit 1), CR(bit 2) and ER(bit 3), the same authorization
/// the original Rust-Shyper bring-up programmed. It only opens the EL0 access
/// path; this benchmark's own tasks run at EL1 and read the counters there, so
/// the value is retained for parity rather than because the workload drops to
/// EL0.
const ARMV8_PMUSERENR_EL0_ACCESS: u64 = 0xf;

/// Writes the writable bits of `val` to `PMCR_EL0`.
fn armv8_pmcr_set(val: u64) {
    let val = val & ARMV8_PMCR_MASK;
    isb();
    // SAFETY: `PMCR_EL0` is a read/write 64-bit control register, so `in(reg)`
    // passes a register-width value. The preceding `isb` orders this write after
    // the reads that computed `val`. The single main benchmark task is the only
    // writer of PMCR here, and the register is reachable at EL1 under the
    // module-level board PMU premise.
    unsafe { core::arch::asm!("msr pmcr_el0, {}", in(reg) val) };
}

/// Enables the 64-bit PMU cycle counter and grants EL0 read access.
///
/// Called once by the main task before any switch measurement.
pub fn enable_cpu_cycle() {
    println!("Enable User Access PMU @ CPU {}\n", get_cpu_id());

    // SAFETY: `PMUSERENR_EL0` is a read/write 64-bit register, so `in(reg)`
    // passes a register-width value. Writing the constant grants EL0 read
    // access; the write runs at EL1 under the module-level board PMU premise,
    // and the single main task is the only writer of this register in the
    // benchmark.
    unsafe { core::arch::asm!("msr pmuserenr_el0, {0:x}", in(reg) ARMV8_PMUSERENR_EL0_ACCESS) };

    armv8_pmcr_set(ARMV8_PMCR_LC | ARMV8_PMCR_E);

    // SAFETY: `PMCNTENSET_EL0` is a read/write 64-bit register, so `in(reg)`
    // passes a register-width value; bit 31 enables the cycle counter. The write
    // runs at EL1 under the module-level board PMU premise, and the single main
    // task is the only writer of this register in the benchmark.
    unsafe { core::arch::asm!("msr pmcntenset_el0, {0:x}", in(reg) (1_u64 << 31)) };

    armv8_pmcr_set(armv8_pmcr() | ARMV8_PMCR_E | ARMV8_PMCR_LC);
}

/// Reads the PMU cycle counter `PMCCNTR_EL0`.
///
/// `enable_cpu_cycle` keeps `PMCR_EL0.LC` set, so the counter is 64-bit wide and
/// does not wrap during a measured round.
pub fn cpu_cycle() -> u64 {
    let value: u64;
    isb();
    // SAFETY: `PMCCNTR_EL0` is a read-only 64-bit counter register, reachable at
    // EL1 under the module-level board PMU premise. The preceding `isb` prevents
    // the processor from reordering this read across the PMU control-register
    // writes, and the counter runs free in 64-bit mode (PMCR_EL0.LC), so no wrap
    // handling is needed within a round.
    unsafe { core::arch::asm!("mrs {0}, pmccntr_el0", out(reg) value) };
    value
}

/// Enables every PMU event counter for the bring-up log.
pub fn enable_pmu_all() {
    let mut val = armv8_pmcr();
    val |= ARMV8_PMCR_E | ARMV8_PMCR_X;
    armv8_pmcr_set(val);
}

/// Resets every PMU counter and the cycle counter before measuring.
pub fn reset_pmu_all() {
    let mut val = armv8_pmcr();
    val |= ARMV8_PMCR_P | ARMV8_PMCR_C;
    armv8_pmcr_set(val);
}
