use ax_cpu::registers;

use super::*;

pub(super) const CURRENT_MODEL: ArchitectureCurrentModel = ArchitectureCurrentModel {
    linux_current: CurrentContextSource::ArchitectureRegister,
    unikernel_tls: CurrentContextSource::RuntimeAnchor,
};

pub(super) struct Backend;

impl ArchitectureRegisterBackend for Backend {}

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, boot_context: usize) {
    // SAFETY: the caller owns offline CPU initialization with traps disabled.
    unsafe {
        if cfg!(kernel_tls) {
            registers::write_sscratch(area_base);
        } else {
            registers::write_tp(boot_context);
            registers::write_sscratch(0);
        }
    }
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    if cfg!(kernel_tls) {
        Ok(registers::read_sscratch())
    } else {
        let current = registers::read_tp();
        if current == 0 {
            return Ok(0);
        }
        // SAFETY: LinuxCurrent tp is written only with a pinned current header.
        unsafe { &*(current as *const ExecutionContextHeader) }
            .cpu_area_base()
            .ok_or(CpuLocalError::CurrentContextMismatch)
    }
}

pub(super) unsafe fn read_current_context(area_base: usize) -> usize {
    if cfg!(kernel_tls) {
        unsafe { area_runtime_anchor(area_base) }.current_context_raw()
    } else {
        registers::read_tp()
    }
}

pub(super) unsafe fn write_current_context(value: usize) {
    if !cfg!(kernel_tls) {
        unsafe { registers::write_tp(value) };
    }
}

#[cfg(kernel_tls)]
pub(super) unsafe fn read_kernel_tls() -> usize {
    registers::read_thread_pointer().as_usize()
}

#[cfg(kernel_tls)]
pub(super) unsafe fn write_kernel_tls(value: usize) {
    unsafe { registers::write_thread_pointer(ax_cpu::context::KernelTlsBase::new(value)) };
}

unsafe fn area_runtime_anchor(area_base: usize) -> &'static crate::CpuRuntimeAnchor {
    unsafe {
        &*((area_base + crate::CPU_AREA_RUNTIME_ANCHOR_OFFSET) as *const crate::CpuRuntimeAnchor)
    }
}
