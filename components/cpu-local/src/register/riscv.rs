use super::*;

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, boot_thread: usize) {
    if cfg!(feature = "tls") {
        unsafe { core::arch::asm!("csrw sscratch, {base}", base = in(reg) area_base) };
    } else {
        unsafe {
            core::arch::asm!(
                "mv tp, {current}",
                "csrw sscratch, zero",
                current = in(reg) boot_thread,
            )
        };
    }
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    if cfg!(feature = "tls") {
        let area_base: usize;
        unsafe { core::arch::asm!("csrr {base}, sscratch", base = out(reg) area_base) };
        Ok(area_base)
    } else {
        let current: usize;
        unsafe { core::arch::asm!("mv {current}, tp", current = out(reg) current) };
        if current == 0 {
            return Ok(0);
        }
        // SAFETY: LinuxCurrent tp is written only with a pinned current header.
        unsafe { &*(current as *const CurrentThreadHeader) }
            .cpu_area_base()
            .ok_or(CpuLocalError::CurrentThreadMismatch)
    }
}

pub(super) unsafe fn read_current_thread(area_base: usize) -> usize {
    if cfg!(feature = "tls") {
        unsafe { area_runtime_anchor(area_base) }.current_thread_raw()
    } else {
        let current: usize;
        unsafe { core::arch::asm!("mv {current}, tp", current = out(reg) current) };
        current
    }
}

pub(super) unsafe fn write_current_thread(value: usize) {
    if !cfg!(feature = "tls") {
        unsafe { core::arch::asm!("mv tp, {value}", value = in(reg) value) };
    }
}

#[cfg(feature = "tls")]
pub(super) unsafe fn read_kernel_tls() -> usize {
    let value: usize;
    unsafe { core::arch::asm!("mv {value}, tp", value = out(reg) value) };
    value
}

#[cfg(feature = "tls")]
pub(super) unsafe fn write_kernel_tls(value: usize) {
    unsafe { core::arch::asm!("mv tp, {value}", value = in(reg) value) };
}

unsafe fn area_runtime_anchor(area_base: usize) -> &'static crate::CpuRuntimeAnchor {
    unsafe {
        &*((area_base + crate::CPU_AREA_RUNTIME_ANCHOR_OFFSET) as *const crate::CpuRuntimeAnchor)
    }
}
