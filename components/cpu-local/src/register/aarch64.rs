use super::*;

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
    if !cfg!(feature = "tls") {
        unsafe { core::arch::asm!("msr SP_EL0, {current}", current = in(reg) boot_thread) };
    }
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

pub(super) unsafe fn read_current_thread(area_base: usize) -> usize {
    if cfg!(feature = "tls") {
        unsafe { area_runtime_anchor(area_base) }.current_thread_raw()
    } else {
        let current: usize;
        unsafe { core::arch::asm!("mrs {current}, SP_EL0", current = out(reg) current) };
        current
    }
}

pub(super) unsafe fn write_current_thread(value: usize) {
    if !cfg!(feature = "tls") {
        unsafe { core::arch::asm!("msr SP_EL0, {value}", value = in(reg) value) };
    }
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

unsafe fn area_runtime_anchor(area_base: usize) -> &'static crate::CpuRuntimeAnchor {
    unsafe {
        &*((area_base + crate::CPU_AREA_RUNTIME_ANCHOR_OFFSET) as *const crate::CpuRuntimeAnchor)
    }
}
