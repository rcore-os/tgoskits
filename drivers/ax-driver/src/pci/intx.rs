//! Move-only ownership of a PCI INTx endpoint gate.

use alloc::sync::Arc;

use ax_kspin::SpinRaw;
use pcie::{CommandRegister, Endpoint};

use crate::{BindingInfo, IrqBindingError, IrqBindingLease};

/// Keeps one PCI endpoint masked until the runtime owns its IRQ action.
///
/// The controller's device-side source and the parent interrupt route remain
/// separate capabilities. This lease owns only the endpoint's INTx gate and
/// can therefore be composed with either a single block device or a
/// multi-device controller bundle.
pub struct PciIntxIrqLease {
    endpoint: SharedPciEndpoint,
    binding: BindingInfo,
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

    pub(super) const fn from_shared(endpoint: SharedPciEndpoint, binding: BindingInfo) -> Self {
        Self { endpoint, binding }
    }
}

impl IrqBindingLease for PciIntxIrqLease {
    fn binding_info(&self) -> BindingInfo {
        self.binding.clone()
    }

    fn enable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.endpoint.update_command(unmask_intx_command);
        Ok(())
    }

    fn disable_binding_irq(&self) -> Result<(), IrqBindingError> {
        self.endpoint.update_command(mask_intx_command);
        Ok(())
    }
}

impl Drop for PciIntxIrqLease {
    fn drop(&mut self) {
        self.endpoint.update_command(mask_intx_command);
    }
}

#[derive(Clone)]
pub(super) struct SharedPciEndpoint(Arc<SpinRaw<Endpoint>>);

impl SharedPciEndpoint {
    pub(super) fn new(endpoint: Endpoint) -> Self {
        Self(Arc::new(SpinRaw::new(endpoint)))
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
