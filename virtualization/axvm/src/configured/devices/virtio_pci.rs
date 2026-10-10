//! AxVM-owned adapter from VirtIO transport state to a generic PCI endpoint.

use core::sync::atomic::AtomicBool;
use std::{
    sync::{Arc, Mutex},
    vec::Vec,
};

use axdevice::{PciCommandRevision, PciConfigEffectId};
use axdevice_base::{DeviceResult, DmaGrant, IrqLine, Resource};
use axvirtio_common::pci::{VIRTIO_PCI_CONFIG_EFFECT_ID, VirtioDeviceCore, VirtioPciTransport};

#[cfg(test)]
use crate::sync::MutexExt;

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
    pub(super) command_revision: Mutex<Option<PciCommandRevision>>,
    pub(super) queue_pending: Arc<AtomicBool>,
    pub(super) work_port: Option<crate::services::DeviceWorkPort>,
    #[cfg(test)]
    pub(super) command_revision_hook: Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl<D: VirtioDeviceCore> VirtioPciFunction<D> {
    /// Creates an endpoint with its bundle-owned DMA grant and INTx source.
    ///
    /// # Errors
    ///
    /// Returns an error when the core's queue configuration is unsupported.
    pub fn try_new(
        transport_core: D,
        dma_grant: DmaGrant,
        irq_line: IrqLine,
    ) -> DeviceResult<Self> {
        Self::try_new_with_queue_pending(
            transport_core,
            dma_grant,
            irq_line,
            Arc::new(AtomicBool::new(false)),
        )
    }

    pub(super) fn try_new_with_queue_pending(
        transport_core: D,
        dma_grant: DmaGrant,
        irq_line: IrqLine,
        queue_pending: Arc<AtomicBool>,
    ) -> DeviceResult<Self> {
        Self::try_new_with_queue_pending_and_work_port(
            transport_core,
            dma_grant,
            irq_line,
            queue_pending,
            None,
        )
    }

    pub(super) fn try_new_with_queue_pending_and_work_port(
        transport_core: D,
        dma_grant: DmaGrant,
        irq_line: IrqLine,
        queue_pending: Arc<AtomicBool>,
        work_port: Option<crate::services::DeviceWorkPort>,
    ) -> DeviceResult<Self> {
        Ok(Self {
            transport: VirtioPciTransport::try_new(transport_core)?,
            dma_grant,
            irq_line,
            resources: Vec::new(),
            command_revision: Mutex::new(None),
            queue_pending,
            work_port,
            #[cfg(test)]
            command_revision_hook: Mutex::new(None),
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
        *self.command_revision_hook.lock_unpoisoned() = Some(Arc::new(hook));
    }

    #[cfg(test)]
    pub(super) fn notify_command_revision_hook(&self) {
        let hook = self.command_revision_hook.lock_unpoisoned().clone();
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
