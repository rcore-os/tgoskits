use core::cell::Cell;

use super::*;

std::thread_local! {
    static CPU_BASE: Cell<usize> = const { Cell::new(0) };
    static KERNEL_TLS: Cell<usize> = const { Cell::new(0) };
    static CPU_BASE_READS: Cell<usize> = const { Cell::new(0) };
    static CURRENT_THREAD_READS: Cell<usize> = const { Cell::new(0) };
    #[cfg(test)]
    static MIGRATION_TARGET: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, _boot_thread: usize) {
    CPU_BASE.set(area_base);
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    CPU_BASE_READS.set(CPU_BASE_READS.get().wrapping_add(1));
    Ok(CPU_BASE.get())
}

pub(super) unsafe fn read_current_thread(_area_base: usize) -> usize {
    CURRENT_THREAD_READS.set(CURRENT_THREAD_READS.get().wrapping_add(1));
    #[cfg(test)]
    MIGRATION_TARGET.with(|target| {
        let target = target.replace(0);
        if target != 0 {
            CPU_BASE.set(target);
        }
    });
    // Host tests execute on x86_64, whose current pointer is the GS runtime
    // anchor itself. Read the live modeled CPU, rather than a previously
    // sampled base, so tests can reproduce migration between the two reads.
    let area_base = CPU_BASE.get();
    if area_base == 0 {
        return 0;
    }
    unsafe { area_runtime_anchor(area_base) }.current_thread_raw()
}

pub(super) unsafe fn write_current_thread(_value: usize) {}

#[cfg(feature = "tls")]
pub(super) unsafe fn read_kernel_tls() -> usize {
    KERNEL_TLS.get()
}

#[cfg(feature = "tls")]
pub(super) unsafe fn write_kernel_tls(value: usize) {
    KERNEL_TLS.set(value);
}

pub(super) fn reset_register_read_counts() {
    CPU_BASE_READS.set(0);
    CURRENT_THREAD_READS.set(0);
}

pub(super) fn register_read_counts() -> super::host_test::RegisterReadCounts {
    super::host_test::RegisterReadCounts {
        cpu_base: CPU_BASE_READS.get(),
        current_thread: CURRENT_THREAD_READS.get(),
    }
}

unsafe fn area_runtime_anchor(area_base: usize) -> &'static crate::CpuRuntimeAnchor {
    unsafe {
        &*((area_base + crate::CPU_AREA_RUNTIME_ANCHOR_OFFSET) as *const crate::CpuRuntimeAnchor)
    }
}

#[cfg(test)]
pub(super) fn migrate_on_next_current_read(area_base: usize) {
    MIGRATION_TARGET.set(area_base);
}
