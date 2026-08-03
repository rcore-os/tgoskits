use core::sync::atomic::{AtomicU32, Ordering};

use ax_kspin::SpinNoIrq;
use irq_framework::{HwIrq, IrqDomainId, IrqId};
use mmio_api::MmioRaw;

pub(crate) const LIOINTC_VECTOR_COUNT: usize = 32;
pub(crate) const LIOINTC_PARENT_COUNT: usize = 4;
pub(crate) const REG_ENABLE: usize = 0x28;
pub(crate) const REG_DISABLE: usize = 0x2c;

/// Shutdown-lifetime LIOINTC state used by hard IRQ dispatch.
pub(crate) struct LioIntcFastPath {
    domain: IrqDomainId,
    regs: MmioRaw,
    isr: MmioRaw,
    parent_irqs: [Option<usize>; LIOINTC_PARENT_COUNT],
    enabled: AtomicU32,
    control: SpinNoIrq<()>,
}

impl LioIntcFastPath {
    pub(crate) fn new(
        domain: IrqDomainId,
        regs: MmioRaw,
        isr: MmioRaw,
        parent_irqs: [Option<usize>; LIOINTC_PARENT_COUNT],
    ) -> Self {
        Self {
            domain,
            regs,
            isr,
            parent_irqs,
            enabled: AtomicU32::new(0),
            control: SpinNoIrq::new(()),
        }
    }

    pub(crate) fn claim_irq(&self, raw: usize) -> Option<IrqId> {
        if !self.parent_irqs.into_iter().flatten().any(|irq| irq == raw) {
            return None;
        }

        // Claim runs in hard IRQ context and must remain independent of the
        // task-side controller lock. Acquire observes the enable publication
        // after its W1 MMIO write without blocking interrupt dispatch.
        let pending = self.isr.read::<u32>(0) & self.enabled.load(Ordering::Acquire);
        (pending != 0).then(|| IrqId::new(self.domain, HwIrq(pending.trailing_zeros())))
    }

    pub(crate) fn complete_irq(&self, irq: IrqId) {
        if irq.domain != self.domain || irq.hwirq.0 as usize >= LIOINTC_VECTOR_COUNT {
            log::warn!("ignore completion for invalid LIOINTC IRQ {irq:?}");
        }
        // Inputs are level-triggered; the device-side handler deasserts them.
    }

    pub(crate) fn set_enabled(&self, input: usize, enabled: bool) {
        debug_assert!(input < LIOINTC_VECTOR_COUNT);
        let _control = self.control.lock();
        let mask = 1u32 << input;
        if enabled {
            self.regs.write(REG_ENABLE, mask);
            self.enabled.fetch_or(mask, Ordering::Release);
        } else {
            self.enabled.fetch_and(!mask, Ordering::AcqRel);
            self.regs.write(REG_DISABLE, mask);
        }
    }

    #[cfg(test)]
    pub(crate) fn claim_irq_while_control_busy(&self, raw: usize) -> Option<IrqId> {
        let _control = self.control.lock();
        self.claim_irq(raw)
    }

    #[cfg(test)]
    pub(crate) fn enabled_mask(&self) -> u32 {
        self.enabled.load(Ordering::Acquire)
    }
}
