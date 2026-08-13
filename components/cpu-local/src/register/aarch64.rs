use super::*;

pub(super) const CURRENT_MODEL: ArchitectureCurrentModel = ArchitectureCurrentModel {
    current_source_aliases_kernel_tls: false,
};

fn current_el() -> Result<usize, CpuLocalError> {
    let current_el: usize;
    unsafe { core::arch::asm!("mrs {value}, CurrentEL", value = out(reg) current_el) };
    let level = (current_el >> 2) & 0b11;
    if matches!(level, 1 | 2) {
        Ok(level)
    } else {
        Err(CpuLocalError::UnsupportedHostLevel { level })
    }
}

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    current_el().map(|_| ())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, boot_thread: usize) {
    match current_el().unwrap_or_else(|_| super::fatal_register_invariant()) {
        1 => unsafe { core::arch::asm!("msr TPIDR_EL1, {base}", base = in(reg) area_base) },
        2 => unsafe { core::arch::asm!("msr TPIDR_EL2, {base}", base = in(reg) area_base) },
        _ => unreachable!(),
    }
    unsafe { core::arch::asm!("msr SP_EL0, {current}", current = in(reg) boot_thread) };
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    let area_base: usize;
    match current_el()? {
        1 => unsafe { core::arch::asm!("mrs {base}, TPIDR_EL1", base = out(reg) area_base) },
        2 => unsafe { core::arch::asm!("mrs {base}, TPIDR_EL2", base = out(reg) area_base) },
        _ => unreachable!(),
    }
    Ok(area_base)
}

pub(super) unsafe fn read_current_thread(_area_base: usize) -> usize {
    let current: usize;
    unsafe { core::arch::asm!("mrs {current}, SP_EL0", current = out(reg) current) };
    current
}

pub(super) unsafe fn write_current_thread(value: usize) {
    unsafe { core::arch::asm!("msr SP_EL0, {value}", value = in(reg) value) };
}

#[cfg(feature = "tls")]
pub(super) unsafe fn read_kernel_tls() -> usize {
    let value: usize;
    unsafe { core::arch::asm!("mrs {value}, TPIDR_EL0", value = out(reg) value) };
    value
}

#[cfg(feature = "tls")]
pub(super) unsafe fn write_kernel_tls(value: usize) {
    unsafe { core::arch::asm!("msr TPIDR_EL0, {value}", value = in(reg) value) };
}
