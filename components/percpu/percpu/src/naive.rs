//! Naive implementation for single CPU use.
use crate::InitError;

/// Returns the per-CPU data area size for one CPU.
///
/// Always returns `0` for "sp-naive" use.
pub fn percpu_area_size() -> usize {
    0
}

/// Returns the per-CPU data area size for the given number of CPUs.
///
/// Always returns `0` for "sp-naive" mode since no per-CPU memory areas are needed.
pub fn percpu_area_layout_expected(_cpu_count: usize) -> core::alloc::Layout {
    core::alloc::Layout::from_size_align(0, 0x40).unwrap()
}

/// Returns the number of per-CPU data areas reserved.
///
/// Always returns `1` for "sp-naive" use.
pub fn percpu_area_num() -> usize {
    1
}

/// Returns the base address of the per-CPU data area on the given CPU.
///
/// Always returns `0` for "sp-naive" use.
pub fn percpu_area_base(_cpu_id: usize) -> usize {
    0
}

/// Reads the architecture-specific per-CPU data register.
///
/// Always returns `0` for "sp-naive" use.
pub fn read_percpu_reg() -> usize {
    0
}

/// Writes the architecture-specific per-CPU data register.
///
/// No effect for "sp-naive" use.
///
/// # Safety
///
/// This function is marked as `unsafe` for consistency with non "sp-naive"
/// implementations.
pub unsafe fn write_percpu_reg(_tp: usize) {}

/// Initializes the per-CPU data register.
///
/// No effect for "sp-naive" use.
pub fn init_percpu_reg(_cpu_id: usize) {}

/// Initialize per-CPU data areas.
///
/// For "sp-naive" mode, no initialization is needed since per-CPU data is stored
/// in global variables. Always returns `1`.
///
/// Returns the number of areas initialized.
pub fn init_in_place() -> Result<usize, InitError> {
    Ok(1)
}

/// Initialize per-CPU data areas with user-provided memory.
///
/// For "sp-naive" mode, parameters are ignored since per-CPU data is stored
/// in global variables. Always returns `1`.
///
/// # Arguments
/// - `_base`: Base address (ignored)
/// - `_cpu_count`: Number of CPUs (ignored)
///
/// Returns the number of areas initialized.
pub fn init(_base: *mut u8, _cpu_count: usize) -> Result<usize, InitError> {
    Ok(1)
}
