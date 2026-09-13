//! Host IRQ exclusion around VGIC and CPU machine-state transfer.

use core::marker::PhantomData;

/// Retains the previous IRQ mask through one non-blocking pinned host scope.
#[must_use = "dropping this guard restores the previous IRQ mask"]
pub struct ArmHostIrqGuard {
    restore_unmasked: bool,
    _not_send_sync: PhantomData<*mut ()>,
}

impl ArmHostIrqGuard {
    /// Masks local IRQs. The enclosing vCPU scope retains CPU pinning.
    pub fn mask() -> Self {
        let restore_unmasked = ax_cpu::interrupt::irqs_enabled();
        ax_cpu::interrupt::disable_irqs();
        Self {
            restore_unmasked,
            _not_send_sync: PhantomData,
        }
    }
}

impl Drop for ArmHostIrqGuard {
    fn drop(&mut self) {
        if self.restore_unmasked {
            ax_cpu::interrupt::enable_irqs();
        }
    }
}
