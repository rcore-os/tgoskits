//! Move-only ownership of a PCI INTx endpoint gate.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use ax_kspin::SpinNoIrq;
use pcie::{CommandRegister, Endpoint};
use rdif_irq::MaskedSource;

use crate::{BindingInfo, IrqBindingError, IrqBindingLease};

/// Keeps one PCI endpoint masked until the runtime owns its IRQ action.
///
/// The controller's device-side source and the parent interrupt route remain
/// separate capabilities. This lease owns only the endpoint's INTx gate and
/// can therefore be composed with either a single block device or a
/// multi-device controller bundle.
pub struct PciIntxIrqLease {
    binding: BindingInfo,
    source_state: Arc<PciIntxSourceState>,
}

impl PciIntxIrqLease {
    /// Masks and retains an endpoint after command-free controller discovery.
    pub fn new(mut endpoint: Endpoint, binding: BindingInfo) -> Self {
        endpoint.update_command(mask_intx_command);
        Self::from_shared(SharedPciEndpoint::new(endpoint), binding)
    }

    /// Masks an endpoint before discovery reads any controller state.
    pub fn mask_for_discovery(endpoint: &mut Endpoint) {
        endpoint.update_command(mask_intx_command);
    }

    pub(super) fn from_shared(endpoint: SharedPciEndpoint, binding: BindingInfo) -> Self {
        Self {
            source_state: Arc::new(PciIntxSourceState::new(endpoint.clone())),
            binding,
        }
    }

    /// Splits out the move-only hard-IRQ masking capability for this endpoint.
    pub fn take_source_mask(&self) -> Option<PciIntxSourceMask> {
        self.source_state
            .source_mask_taken
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .ok()?;
        Some(PciIntxSourceMask {
            state: Arc::clone(&self.source_state),
        })
    }

    /// Rearms a source token only when it belongs to the active INTx epoch.
    pub fn rearm_source(&self, source: MaskedSource) -> bool {
        self.source_state.rearm(source)
    }
}

impl IrqBindingLease for PciIntxIrqLease {
    fn binding_info(&self) -> BindingInfo {
        self.binding.clone()
    }

    fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.source_state.enable_new_epoch();
        Ok(())
    }

    fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.source_state.disable_and_invalidate();
        Ok(())
    }
}

impl Drop for PciIntxIrqLease {
    fn drop(&mut self) {
        self.source_state.disable_and_invalidate();
    }
}

/// Move-only hard-IRQ capability for masking one PCI endpoint's INTx source.
///
/// It deliberately exposes no unmask operation. Only the task-owned
/// [`PciIntxIrqLease`] can generation-check and rearm a contained source.
pub struct PciIntxSourceMask {
    state: Arc<PciIntxSourceState>,
}

impl PciIntxSourceMask {
    /// Masks the precise endpoint source without consulting the IRQ framework.
    ///
    /// The maintenance owner performs every task-side endpoint access with
    /// local IRQs disabled. The registered callback runs on that same fixed
    /// CPU. The shared endpoint uses an IRQ-save lock, so capture cannot
    /// interrupt task-side PCI configuration while that lock is held.
    pub fn mask_from_irq(&mut self) -> MaskedSource {
        self.state.mask_from_irq()
    }
}

struct PciIntxSourceState {
    endpoint: SharedPciEndpoint,
    generation: AtomicU64,
    enabled: AtomicBool,
    irq_masked: AtomicBool,
    source_mask_taken: AtomicBool,
}

impl PciIntxSourceState {
    const SOURCE_BITMAP: u64 = 1;

    fn new(endpoint: SharedPciEndpoint) -> Self {
        Self {
            endpoint,
            generation: AtomicU64::new(1),
            enabled: AtomicBool::new(false),
            irq_masked: AtomicBool::new(false),
            source_mask_taken: AtomicBool::new(false),
        }
    }

    fn enable_new_epoch(&self) {
        self.advance_generation();
        self.irq_masked.store(false, Ordering::Release);
        self.enabled.store(true, Ordering::Release);
        self.endpoint.update_command(unmask_intx_command);
    }

    fn disable_and_invalidate(&self) {
        self.enabled.store(false, Ordering::Release);
        self.endpoint.update_command(mask_intx_command);
        self.irq_masked.store(false, Ordering::Release);
        self.advance_generation();
    }

    fn mask_from_irq(&self) -> MaskedSource {
        self.enabled.store(false, Ordering::Release);
        self.endpoint.update_command(mask_intx_command);
        self.irq_masked.store(true, Ordering::Release);
        MaskedSource::try_new(self.generation.load(Ordering::Acquire), Self::SOURCE_BITMAP)
            .expect("PCI INTx source generation and bitmap are always nonzero")
    }

    fn rearm(&self, source: MaskedSource) -> bool {
        if source.bitmap().get() != Self::SOURCE_BITMAP
            || source.generation().get() != self.generation.load(Ordering::Acquire)
            || self
                .irq_masked
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire)
                .is_err()
        {
            return false;
        }
        self.enabled.store(true, Ordering::Release);
        self.endpoint.update_command(unmask_intx_command);
        true
    }

    fn advance_generation(&self) {
        let mut current = self.generation.load(Ordering::Relaxed);
        loop {
            let mut next = current.wrapping_add(1);
            if next == 0 {
                next = 1;
            }
            match self.generation.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(observed) => current = observed,
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct SharedPciEndpoint(Arc<SpinNoIrq<Endpoint>>);

impl SharedPciEndpoint {
    pub(super) fn new(endpoint: Endpoint) -> Self {
        Self(Arc::new(SpinNoIrq::new(endpoint)))
    }

    pub(super) fn update_command(&self, update: impl FnOnce(CommandRegister) -> CommandRegister) {
        self.0.lock().update_command(update);
    }

    #[cfg(virtio_dev)]
    pub(super) fn read(&self, offset: u16) -> u32 {
        self.0.lock().read(offset)
    }

    #[cfg(virtio_dev)]
    pub(super) fn write(&self, offset: u16, value: u32) {
        self.0.lock().write(offset, value);
    }
}

fn mask_intx_command(mut command: CommandRegister) -> CommandRegister {
    command.insert(CommandRegister::INTERRUPT_DISABLE);
    command
}

fn unmask_intx_command(mut command: CommandRegister) -> CommandRegister {
    command.remove(CommandRegister::INTERRUPT_DISABLE);
    command
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intx_command_transitions_preserve_unrelated_endpoint_capabilities() {
        let original = CommandRegister::MEMORY_ENABLE | CommandRegister::BUS_MASTER_ENABLE;
        let masked = mask_intx_command(original);
        assert!(masked.contains(CommandRegister::INTERRUPT_DISABLE));
        assert!(masked.contains(CommandRegister::MEMORY_ENABLE));
        assert!(masked.contains(CommandRegister::BUS_MASTER_ENABLE));

        let unmasked = unmask_intx_command(masked);
        assert!(!unmasked.contains(CommandRegister::INTERRUPT_DISABLE));
        assert!(unmasked.contains(CommandRegister::MEMORY_ENABLE));
        assert!(unmasked.contains(CommandRegister::BUS_MASTER_ENABLE));
    }
}
