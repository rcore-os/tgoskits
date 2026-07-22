use core::cell::Cell;

use super::*;

std::thread_local! {
    static CPU_BASE: Cell<usize> = const { Cell::new(0) };
    static KERNEL_TLS: Cell<usize> = const { Cell::new(0) };
}

pub(super) fn validate_environment() -> Result<(), CpuLocalError> {
    Ok(())
}

pub(super) unsafe fn install_cpu_base(area_base: usize, _boot_thread: usize) {
    CPU_BASE.set(area_base);
}

pub(super) unsafe fn read_cpu_base() -> Result<usize, CpuLocalError> {
    Ok(CPU_BASE.get())
}

pub(super) unsafe fn read_current_thread(area_base: usize) -> usize {
    // Host tests execute on x86_64, whose current pointer is the GS runtime
    // anchor itself. Modeling that source directly also lets IRQ worker
    // threads attach to the same serialized CPU fixture without inventing a
    // second thread-local current-task state source.
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

unsafe fn area_runtime_anchor(area_base: usize) -> &'static crate::CpuRuntimeAnchor {
    unsafe {
        &*((area_base + crate::CPU_AREA_RUNTIME_ANCHOR_OFFSET) as *const crate::CpuRuntimeAnchor)
    }
}
