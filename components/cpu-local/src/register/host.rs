use core::cell::Cell;

use super::*;

pub(super) const CURRENT_MODEL: ArchitectureCurrentModel = ArchitectureCurrentModel {
    linux_current: CurrentContextSource::ArchitectureRegister,
    unikernel_tls: CurrentContextSource::RuntimeAnchor,
};

std::thread_local! {
    static CPU_BASE: Cell<usize> = const { Cell::new(0) };
    static ARCHITECTURE_CURRENT: Cell<usize> = const { Cell::new(0) };
    static KERNEL_TLS: Cell<usize> = const { Cell::new(0) };
    #[cfg(test)]
    static MIGRATION_TARGET: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, boot_context: usize) {
    CPU_BASE.set(area_base);
    ARCHITECTURE_CURRENT.set(if cfg!(feature = "tls") {
        0
    } else {
        boot_context
    });
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    Ok(CPU_BASE.get())
}

pub(super) unsafe fn read_current_context(area_base: usize) -> usize {
    #[cfg(test)]
    MIGRATION_TARGET.with(|target| {
        let target = target.replace(0);
        if target != 0 {
            CPU_BASE.set(target);
        }
    });
    if cfg!(feature = "tls") {
        // SAFETY: the shared caller validated the sampled shutdown-lifetime
        // CPU area before asking the host backend for its selected source.
        return unsafe { area_runtime_anchor(area_base) }.current_context_raw();
    }
    ARCHITECTURE_CURRENT
        .with(|current| {
            let current = current.get();
            (current != 0).then_some(current)
        })
        .unwrap_or(0)
}

pub(super) unsafe fn write_current_context(value: usize) {
    ARCHITECTURE_CURRENT.set(value);
}

#[cfg(feature = "tls")]
pub(super) unsafe fn read_kernel_tls() -> usize {
    KERNEL_TLS.get()
}

#[cfg(feature = "tls")]
pub(super) unsafe fn write_kernel_tls(value: usize) {
    KERNEL_TLS.set(value);
}

#[cfg(test)]
pub(super) fn migrate_on_next_current_read(area_base: usize) {
    MIGRATION_TARGET.set(area_base);
}

#[cfg(all(test, not(feature = "tls")))]
pub(super) fn set_architecture_current(current: usize) {
    ARCHITECTURE_CURRENT.set(current);
}

unsafe fn area_runtime_anchor(area_base: usize) -> &'static crate::CpuRuntimeAnchor {
    // SAFETY: forwarded caller contract supplies a validated CPU-area base.
    unsafe {
        &*((area_base + crate::CPU_AREA_RUNTIME_ANCHOR_OFFSET) as *const crate::CpuRuntimeAnchor)
    }
}
