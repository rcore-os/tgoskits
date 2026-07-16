//! Current CPU helpers shared by architecture backends.

use irq_framework::CpuId;

/// Firmware/hardware CPU identity loaded from immutable runtime metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct HardwareCpuId(usize);

impl HardwareCpuId {
    /// Returns the architecture-visible numeric CPU identity.
    pub(crate) const fn as_usize(self) -> usize {
        self.0
    }
}

/// Resolves one logical IPI target without parsing firmware or taking locks.
pub(crate) fn runtime_cpu_target(cpu: CpuId) -> Option<HardwareCpuId> {
    let target = someboot::smp::runtime_cpu_target(cpu.0).ok()?;
    (target.logical_index() == cpu.0).then_some(HardwareCpuId(target.hardware_id()))
}

/// Returns the runtime-owned current logical CPU without an early-boot fallback.
pub(crate) fn runtime_current_cpu() -> Option<CpuId> {
    crate::setup::kernel().current_cpu_idx().map(CpuId)
}

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
