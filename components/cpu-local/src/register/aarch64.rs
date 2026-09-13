use ax_cpu::registers;

use super::*;

pub(super) const CURRENT_MODEL: ArchitectureCurrentModel = ArchitectureCurrentModel {
    linux_current: CurrentContextSource::ArchitectureRegister,
    unikernel_tls: CurrentContextSource::ArchitectureRegister,
};

pub(super) struct Backend;

impl ArchitectureRegisterBackend for Backend {}

fn current_el() -> Result<usize, CpuLocalError> {
    let level = registers::current_exception_level();
    if matches!(level, 1 | 2) {
        Ok(level)
    } else {
        Err(CpuLocalError::UnsupportedHostLevel { level })
    }
}

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    current_el().map(|_| ())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, boot_context: usize) {
    match current_el().unwrap_or_else(|_| super::fatal_register_invariant()) {
        1 => unsafe { registers::write_tpidr_el1(area_base) },
        2 => unsafe { registers::write_tpidr_el2(area_base) },
        _ => unreachable!(),
    }
    unsafe { registers::write_sp_el0(boot_context) };
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    Ok(match current_el()? {
        1 => registers::read_tpidr_el1(),
        2 => registers::read_tpidr_el2(),
        _ => unreachable!(),
    })
}

pub(super) unsafe fn read_current_context(_area_base: usize) -> usize {
    registers::read_sp_el0()
}

pub(super) unsafe fn write_current_context(value: usize) {
    unsafe { registers::write_sp_el0(value) };
}

#[cfg(kernel_tls)]
pub(super) unsafe fn read_kernel_tls() -> usize {
    registers::read_thread_pointer().as_usize()
}

#[cfg(kernel_tls)]
pub(super) unsafe fn write_kernel_tls(value: usize) {
    unsafe { registers::write_thread_pointer(ax_cpu::context::KernelTlsBase::new(value)) };
}
