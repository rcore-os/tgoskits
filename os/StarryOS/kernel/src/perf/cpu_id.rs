//! Typed logical CPU identity shared by perf target and PMU ownership paths.

/// Validated logical CPU target for one perf event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfCpuId(usize);

impl PerfCpuId {
    /// Creates a validated logical CPU id.
    pub(crate) const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the CPU id as an array index.
    #[cfg(target_arch = "aarch64")]
    pub(crate) const fn as_usize(self) -> usize {
        self.0
    }
}
