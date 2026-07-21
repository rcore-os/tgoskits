//! Naive implementation for single CPU use.

/// Returns the per-CPU data area size for one CPU.
///
/// Always returns `0` for "sp-naive" use.
pub(crate) fn percpu_area_size() -> usize {
    0
}

/// Returns the single-CPU template origin for API compatibility.
#[doc(hidden)]
pub(crate) fn percpu_template_base() -> usize {
    0
}

/// Initialize all per-CPU data areas.
///
/// Returns the number of areas initialized.
///
/// For "sp-naive" use it does nothing and returns `1`.
pub fn init() -> usize {
    1
}
