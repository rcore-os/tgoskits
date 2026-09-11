//! AxVM-owned adapter from VirtIO transport state to a generic PCI endpoint.

#[cfg(test)]
use std::sync::Arc;
use std::vec::Vec;

use ax_sync::SpinLock;
use axdevice::{PciCommandRevision, PciConfigEffectId};
use axdevice_base::{DeviceResult, DmaGrant, IrqLine, Resource};
use axvirtio_common::pci::{VIRTIO_PCI_CONFIG_EFFECT_ID, VirtioDeviceCore, VirtioPciTransport};

pub(super) const PCI_CFG_EFFECTS: [PciConfigEffectId; 1] =
    [PciConfigEffectId::new(VIRTIO_PCI_CONFIG_EFFECT_ID)];
const PCI_CFG_DATA_OFFSET: u32 = 16;
const PCI_CFG_DATA_END: u32 = 20;

/// Generic PCI endpoint adapter for a modern VirtIO function.
///
/// PCI topology and BAR placement remain owned by `axdevice`; this adapter
/// only translates an authenticated BAR/config callback into the shared
/// BAR-relative VirtIO transport and endpoint-scoped DMA/IRQ capabilities.
pub struct VirtioPciFunction<D: VirtioDeviceCore> {
    pub(super) transport: VirtioPciTransport<D>,
    pub(super) dma_grant: DmaGrant,
    pub(super) irq_line: IrqLine,
    pub(super) resources: Vec<Resource>,
    pub(super) command_revision: SpinLock<Option<PciCommandRevision>>,
    #[cfg(test)]
    pub(super) command_revision_hook: SpinLock<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl<D: VirtioDeviceCore> VirtioPciFunction<D> {
    /// Creates an endpoint with its bundle-owned DMA grant and INTx source.
    ///
    /// # Errors
    ///
    /// Returns the transport configuration error when the core cannot be
    /// served by the synchronous VirtIO PCI adapter.
    pub fn try_new(
        transport_core: D,
        dma_grant: DmaGrant,
        irq_line: IrqLine,
    ) -> DeviceResult<Self> {
        Ok(Self {
            transport: VirtioPciTransport::try_new(transport_core)?,
            dma_grant,
            irq_line,
            resources: Vec::new(),
            command_revision: SpinLock::new(None),
            #[cfg(test)]
            command_revision_hook: SpinLock::new(None),
        })
    }

    /// Returns the shared transport.
    pub fn transport(&self) -> &VirtioPciTransport<D> {
        &self.transport
    }

    #[cfg(test)]
    pub(super) fn set_command_revision_hook<F>(&self, hook: F)
    where
        F: Fn() + Send + Sync + 'static,
    {
        *self.command_revision_hook.lock() = Some(Arc::new(hook));
    }

    #[cfg(test)]
    pub(super) fn notify_command_revision_hook(&self) {
        let hook = self.command_revision_hook.lock().clone();
        if let Some(hook) = hook {
            hook();
        }
    }
}

mod config;
mod endpoint;
mod interrupt;

pub use config::virtio_capabilities;

#[cfg(test)]
mod tests;
