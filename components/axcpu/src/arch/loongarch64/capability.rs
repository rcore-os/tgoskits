//! LoongArch CPU configuration discovery.

/// Reports the CPUCFG2 LVZ extension bit of the current CPU.
pub fn has_hypervisor_extension() -> bool {
    let config = read_cpucfg(2);
    config & (1 << 10) != 0
}

/// Reads an architectural CPUCFG word without applying operating-system policy.
pub fn read_cpucfg(index: usize) -> usize {
    let value;
    // SAFETY: CPUCFG reads immutable hardware identity and capability information.
    unsafe {
        core::arch::asm!("cpucfg {}, {}", out(reg) value, in(reg) index, options(nomem, nostack));
    }
    value
}
