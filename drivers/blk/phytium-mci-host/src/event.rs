use super::*;

/// Stable controller event extracted from Phytium MCI raw interrupt status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
    #[default]
    None,
    /// A command response has completed.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// The internal DMA engine reached its descriptor boundary.
    ///
    /// This is only one half of a DMA request's terminal condition. The
    /// controller must independently report [`Event::TransferComplete`].
    DmaComplete,
    /// Receive FIFO can be drained.
    ReceiveReady,
    /// Transmit FIFO can accept more data.
    TransmitReady,
    /// One or more controller error bits are pending.
    Error { raw_status: u32 },
    /// One or more internal DMA status bits report a failed transfer.
    DmaError { raw_status: u32 },
    /// Status bits are pending but do not map to a high-level event yet.
    Other { raw_status: u32 },
}

impl HostEvent for Event {
    fn kind(&self) -> HostEventKind {
        match self {
            Event::None => HostEventKind::None,
            Event::CommandComplete => HostEventKind::CommandComplete,
            Event::TransferComplete => HostEventKind::TransferComplete,
            Event::DmaComplete => HostEventKind::Other,
            Event::ReceiveReady => HostEventKind::ReceiveReady,
            Event::TransmitReady => HostEventKind::TransmitReady,
            Event::Error { .. } | Event::DmaError { .. } => HostEventKind::Error,
            Event::Other { .. } => HostEventKind::Other,
        }
    }

    fn source(&self) -> HostEventSource {
        match self {
            Event::CommandComplete => HostEventSource::Command,
            Event::TransferComplete
            | Event::DmaComplete
            | Event::DmaError { .. }
            | Event::ReceiveReady
            | Event::TransmitReady => HostEventSource::Data,
            Event::None | Event::Error { .. } | Event::Other { .. } => HostEventSource::Controller,
        }
    }
}

pub(crate) const MCI_INT_RESPONSE_ERROR: u32 = 1 << 1;
pub(crate) const MCI_INT_COMMAND_DONE: u32 = 1 << 2;
pub(crate) const MCI_INT_DATA_TRANSFER_OVER: u32 = 1 << 3;
pub(crate) const MCI_INT_TXDR: u32 = 1 << 4;
pub(crate) const MCI_INT_RXDR: u32 = 1 << 5;
pub(crate) const MCI_INT_RESPONSE_CRC_ERROR: u32 = 1 << 6;
pub(crate) const MCI_INT_DATA_CRC_ERROR: u32 = 1 << 7;
pub(crate) const MCI_INT_RESPONSE_TIMEOUT: u32 = 1 << 8;
pub(crate) const MCI_INT_DATA_READ_TIMEOUT: u32 = 1 << 9;
pub(crate) const MCI_INT_HOST_TIMEOUT: u32 = 1 << 10;
pub(crate) const MCI_INT_FIFO_UNDER_OVER_RUN: u32 = 1 << 11;
pub(crate) const MCI_INT_HARDWARE_LOCKED_WRITE: u32 = 1 << 12;
pub(crate) const MCI_INT_START_BIT_ERROR: u32 = 1 << 13;
pub(crate) const MCI_INT_END_BIT_ERROR: u32 = 1 << 15;
pub(crate) const MCI_INT_ERROR_MASK: u32 = MCI_INT_RESPONSE_ERROR
    | MCI_INT_RESPONSE_CRC_ERROR
    | MCI_INT_DATA_CRC_ERROR
    | MCI_INT_RESPONSE_TIMEOUT
    | MCI_INT_DATA_READ_TIMEOUT
    | MCI_INT_HOST_TIMEOUT
    | MCI_INT_FIFO_UNDER_OVER_RUN
    | MCI_INT_HARDWARE_LOCKED_WRITE
    | MCI_INT_START_BIT_ERROR
    | MCI_INT_END_BIT_ERROR;

pub(crate) const MCI_IDSTS_TRANSMIT: u32 = 1 << 0;
pub(crate) const MCI_IDSTS_RECEIVE: u32 = 1 << 1;
pub(crate) const MCI_IDSTS_FATAL_BUS_ERROR: u32 = 1 << 2;
pub(crate) const MCI_IDSTS_DESCRIPTOR_UNAVAILABLE: u32 = (1 << 3) | (1 << 4);
pub(crate) const MCI_IDSTS_CARD_ERROR_SUMMARY: u32 = 1 << 5;
pub(crate) const MCI_IDSTS_ABNORMAL_SUMMARY: u32 = 1 << 9;
pub(crate) const MCI_IDSTS_ERROR_MASK: u32 = MCI_IDSTS_FATAL_BUS_ERROR
    | MCI_IDSTS_DESCRIPTOR_UNAVAILABLE
    | MCI_IDSTS_CARD_ERROR_SUMMARY
    | MCI_IDSTS_ABNORMAL_SUMMARY;

/// Hard-IRQ-owned destructive status capture endpoint.
///
/// OS glue moves this endpoint into the IRQ action registered by the same
/// CPU-pinned maintenance owner that retains [`crate::PhytiumMci`] and the
/// matching control endpoint.
pub struct PhytiumMciIrqEndpoint {
    pub(crate) irq: Arc<host::IrqCore>,
}

/// Maintenance-owner capability for generation-checked source rearming.
pub struct PhytiumMciIrqControl {
    pub(crate) irq: Arc<host::IrqCore>,
}

/// Unique split ownership of one Phytium MCI interrupt source.
pub type PhytiumMciIrqSource =
    sdmmc_protocol::sdio::host::SdioIrqSource<PhytiumMciIrqEndpoint, PhytiumMciIrqControl>;
