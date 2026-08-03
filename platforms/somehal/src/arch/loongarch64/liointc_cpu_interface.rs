use core::sync::atomic::{AtomicU32, Ordering};

use irq_framework::{HwIrq, IrqDomainId, IrqId};
use mmio_api::MmioRaw;

pub(crate) const LIOINTC_VECTOR_COUNT: usize = 32;
pub(crate) const LIOINTC_PARENT_COUNT: usize = 4;

/// Shutdown-lifetime LIOINTC CPU interface read by hard IRQ dispatch.
///
/// The task-owned controller may only publish its enabled-input state through
/// the atomic methods below. It does not share register ownership or a lock
/// with this interface.
pub(crate) struct LioIntcCpuInterface {
    domain: IrqDomainId,
    isr: MmioRaw,
    parent_irqs: [Option<usize>; LIOINTC_PARENT_COUNT],
    enabled: AtomicU32,
}

impl LioIntcCpuInterface {
    pub(crate) fn new(
        domain: IrqDomainId,
        isr: MmioRaw,
        parent_irqs: [Option<usize>; LIOINTC_PARENT_COUNT],
    ) -> Self {
        Self {
            domain,
            isr,
            parent_irqs,
            enabled: AtomicU32::new(0),
        }
    }

    pub(crate) fn claim_irq(&self, raw: usize) -> Option<IrqId> {
        if !self.parent_irqs.into_iter().flatten().any(|irq| irq == raw) {
            return None;
        }

        // Acquire observes the controller's enabled publication after its W1
        // MMIO write. The CPU interface has no reference to the task-owned
        // controller, so hard IRQ dispatch cannot depend on its lock.
        let pending = self.isr.read::<u32>(0) & self.enabled.load(Ordering::Acquire);
        (pending != 0).then(|| IrqId::new(self.domain, HwIrq(pending.trailing_zeros())))
    }

    pub(crate) fn complete_irq(&self, irq: IrqId) {
        if irq.domain != self.domain || irq.hwirq.0 as usize >= LIOINTC_VECTOR_COUNT {
            log::warn!("ignore completion for invalid LIOINTC IRQ {irq:?}");
        }
        // Inputs are level-triggered; the device-side handler deasserts them.
    }

    pub(crate) fn publish_enabled(&self, input: usize) {
        debug_assert!(input < LIOINTC_VECTOR_COUNT);
        let mask = 1u32 << input;
        self.enabled.fetch_or(mask, Ordering::Release);
    }

    pub(crate) fn hide_disabled(&self, input: usize) {
        debug_assert!(input < LIOINTC_VECTOR_COUNT);
        let mask = 1u32 << input;
        self.enabled.fetch_and(!mask, Ordering::AcqRel);
    }

    #[cfg(test)]
    pub(crate) fn enabled_mask(&self) -> u32 {
        self.enabled.load(Ordering::Acquire)
    }
}
