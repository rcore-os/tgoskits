//! Current CPU helpers shared by architecture backends.

/// Returns the current logical CPU index.
///
/// This prefers the kernel runtime interface. Before the kernel can provide a
/// runtime answer, it falls back to the early boot CPU-index convention exposed
/// by `someboot`.
pub fn current_cpu_idx() -> Option<usize> {
    crate::setup::kernel()
        .current_cpu_idx()
        .or_else(someboot::smp::try_early_cpu_idx)
}
