//! ARM64 CPU cycle and timer
#![allow(dead_code)]

/// Read reg: MPIDR_EL1
pub fn read_mpidr() -> u64 {
    let reg_r: u64;
    unsafe {
        core::arch::asm!("mrs {}, MPIDR_EL1", out(reg) reg_r);
    }

    reg_r
}

/// Converts MPIDR to CPU ID
pub fn mpidr2cpuid(mpidr: u64) -> usize {
    // Qemu
    #[cfg(feature = "qemu")]
    {
        (mpidr & 0xffffff & 0xff) as usize
    }

    // RK3588
    #[cfg(not(feature = "qemu"))]
    {
        ((mpidr >> 8) & 0xff) as usize
    }
}

pub fn get_cpu_id() -> usize {
    let mpidr = read_mpidr();
    mpidr2cpuid(mpidr)
}

/// CPU Freq = (CPU cycles)/Time (s)
pub fn cpu_freq(cpu_cycle: u64, timer_sum: u64) -> u64 {
    (cpu_cycle / timer_sum) * timer_freq()
}

/// Data Synchronization Barrier
pub fn dsb() {
    unsafe {
        core::arch::asm!("dsb sy");
    }
}

/// Instruction Sync Barrier
pub fn isb() {
    unsafe {
        core::arch::asm!("isb");
    }
}

pub fn timer_cnt() -> u64 {
    let cnt: u64;
    unsafe { core::arch::asm!("mrs {0}, cntvct_el0", out(reg) cnt) };
    cnt
}

pub fn timer_freq() -> u64 {
    let freq: u64;
    unsafe { core::arch::asm!("mrs {0}, cntfrq_el0", out(reg) freq) };
    freq
}

pub fn armv8_pmuserenr() -> u64 {
    let value: u64;
    unsafe { core::arch::asm!("mrs {0}, pmuserenr_el0", out(reg) value) };
    value
}

pub fn armv8_pmcntenset() -> u64 {
    let value: u64;
    unsafe { core::arch::asm!("mrs {0}, pmcntenset_el0", out(reg) value) };
    value
}

pub fn armv8_pmcr() -> u64 {
    let value: u64;
    unsafe { core::arch::asm!("mrs {0}, pmcr_el0", out(reg) value) };
    value
}

const ARMV8_PMCR_MASK: u64 = 0x3f;
const ARMV8_PMCR_E: u64 = 1 << 0; /* Enable all counters */
const ARMV8_PMCR_P: u64 = 1 << 1; /* Reset all counters */
const ARMV8_PMCR_C: u64 = 1 << 2; /* Cycle counter reset */
const ARMV8_PMCR_D: u64 = 1 << 3; /* CCNT counts every 64th cpu cycle */
const ARMV8_PMCR_X: u64 = 1 << 4; /* Export to ETM */
const ARMV8_PMCR_DP: u64 = 1 << 5; /* Disable CCNT if non-invasive debug*/
const ARMV8_PMCR_LC: u64 = 1 << 6; /* Cycle Counter 64bit overflow*/
const ARMV8_PMCR_N_SHIFT: u64 = 11; /* Number of counters supported */
const ARMV8_PMCR_N_MASK: u64 = 0x1f;

const ARMV8_PMUSERENR_EN_EL0: u64 = 1 << 0; /* EL0 access enable */
const ARMV8_PMUSERENR_CR: u64 = 1 << 2; /* Cycle counter read enable */
const ARMV8_PMUSERENR_ER: u64 = 1 << 3; /* Event counter read enable */

pub fn armv8_pmcr_set(val: u64) {
    let val = val & ARMV8_PMCR_MASK;
    isb();
    unsafe { core::arch::asm!("msr pmcr_el0, {}", in(reg) val) };
}

pub fn enable_cpu_cycle() {
    println!("Enable User Access PMU @ CPU {}\n", get_cpu_id());
    // pmuserenr_el0, Enable or disables EL0 access to the performance Monitors
    unsafe { core::arch::asm!("msr pmuserenr_el0, {0:x}", in(reg) 0xf_u64) };
    armv8_pmcr_set(ARMV8_PMCR_LC | ARMV8_PMCR_E);

    // pmcntenset_el0, Enables the Cycle Count Register
    unsafe { core::arch::asm!("msr pmcntenset_el0, {0:x}", in(reg) (1_u64 << 31)) };
    armv8_pmcr_set(armv8_pmcr() | ARMV8_PMCR_E | ARMV8_PMCR_LC);
}

pub fn disable_cpu_cycle() {
    let cpu_id = get_cpu_id();
    println!("Disabling user-mode PMU access on CPU{}", cpu_id);

    // Program PMU and disable all counters
    armv8_pmcr_set(armv8_pmcr() | (!ARMV8_PMCR_E));
    unsafe { core::arch::asm!("msr pmuserenr_el0, {0:x}", in(reg) 0_u64) };
}

/// CPU Cycle Counter
pub fn cpu_cycle() -> u64 {
    let value: u64;
    isb();
    unsafe { core::arch::asm!("mrs {0}, pmccntr_el0", out(reg) value) };
    value
}

// User mode to read ARMv8 PMU

// 2 enable pmu
pub fn enable_pmu_all() {
    // Enable all counters
    let mut val = armv8_pmcr();
    val |= ARMV8_PMCR_E | ARMV8_PMCR_X;
    armv8_pmcr_set(val);
}

// 1 reset pmu
pub fn reset_pmu_all() {
    let mut val = armv8_pmcr();
    val |= ARMV8_PMCR_P | ARMV8_PMCR_C;
    armv8_pmcr_set(val);
}
