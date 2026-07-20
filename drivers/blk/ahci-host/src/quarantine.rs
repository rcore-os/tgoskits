use alloc::sync::Arc;
use core::{fmt, mem::ManuallyDrop};

use dma_api::{InFlightDma, QuarantinedDma};

use crate::{command::PortCommandMemory, irq::HostShared};

/// Why AHCI-owned DMA backing could not be returned to the allocator.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AhciDmaQuarantineReason {
    InitializationAbandoned,
    InitializationStopTimedOut,
    HostAbandoned,
    PortDeviceAbandoned,
    QueueAbandoned,
}

/// Named owner for AHCI memory whose DMA-stop proof was not established.
///
/// The command/FIS memory and HBA mapping remain represented by Rust values,
/// but their destructors are deliberately suppressed. Constructing this type
/// performs no MMIO and does not claim that the controller stopped. Explicit
/// lifecycle transitions are the only path that may recover and release these
/// resources.
#[must_use = "unproven AHCI DMA ownership must remain quarantined"]
pub(crate) struct AhciDmaQuarantine {
    port: usize,
    reason: AhciDmaQuarantineReason,
    controller_cookie: usize,
    data_dma: Option<QuarantinedDma>,
    command_memory: ManuallyDrop<PortCommandMemory>,
    shared: ManuallyDrop<Arc<HostShared>>,
}

impl AhciDmaQuarantine {
    pub(crate) fn new(
        port: usize,
        reason: AhciDmaQuarantineReason,
        controller_cookie: usize,
        command_memory: PortCommandMemory,
        data_dma: Option<InFlightDma>,
        shared: &Arc<HostShared>,
    ) -> Self {
        Self {
            port,
            reason,
            controller_cookie,
            data_dma: data_dma.map(InFlightDma::quarantine),
            command_memory: ManuallyDrop::new(command_memory),
            shared: ManuallyDrop::new(Arc::clone(shared)),
        }
    }
}

impl fmt::Debug for AhciDmaQuarantine {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AhciDmaQuarantine")
            .field("port", &self.port)
            .field("reason", &self.reason)
            .field("controller_cookie", &self.controller_cookie)
            .field("command_list_dma", &self.command_memory.command_list_dma())
            .field("received_fis_dma", &self.command_memory.received_fis_dma())
            .field(
                "data_dma_bytes",
                &self.data_dma.as_ref().map(|dma| dma.len().get()),
            )
            .field("shared_anchor", &Arc::as_ptr(&self.shared))
            .finish_non_exhaustive()
    }
}
