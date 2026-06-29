//! Performance-monitoring capability helpers.

#[cfg(target_arch = "aarch64")]
pub use ax_cpu::pmu::PmuInfo;

#[cfg(not(target_arch = "aarch64"))]
/// Information probed from a CPU PMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PmuInfo {
    /// Number of programmable event counters.
    pub num_counters: usize,
}

/// Returns PMU information when the current architecture/runtime supports it.
pub fn info() -> Option<PmuInfo> {
    #[cfg(target_arch = "aarch64")]
    {
        ax_cpu::pmu::probe()
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        None
    }
}

/// Returns the raw CPU identification register used by PMU tooling.
pub fn cpu_id_raw() -> Option<u64> {
    #[cfg(target_arch = "aarch64")]
    {
        Some(ax_cpu::pmu::read_midr_el1())
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        None
    }
}

/// Returns the platform IRQ id used for PMU overflows.
#[cfg(feature = "irq")]
pub fn irq() -> Result<crate::irq::IrqId, crate::irq::IrqError> {
    #[cfg(target_arch = "aarch64")]
    {
        const PMU_IRQ: crate::irq::HwIrq = crate::irq::HwIrq(23);
        crate::irq::resolve_percpu_irq(PMU_IRQ)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        Err(crate::irq::IrqError::Unsupported)
    }
}
