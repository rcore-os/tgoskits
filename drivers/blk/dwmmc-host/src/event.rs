//! IRQ-owned destructive status acknowledgement and stable event snapshots.

use super::*;

/// Stable controller event extracted from DW_mshc raw interrupt status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
    #[default]
    None,
    /// Task context is publishing a new request epoch; hardware status was
    /// deliberately left untouched so a level IRQ can be retried safely.
    Deferred,
    /// A command response has completed.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// The internal DMA engine has returned descriptor ownership. The request
    /// remains active until the controller also reports data transfer over.
    DmaComplete,
    /// Receive FIFO can be drained.
    ReceiveReady,
    /// Transmit FIFO can accept more data.
    TransmitReady,
    /// One or more controller error bits are pending.
    Error { raw_status: u32 },
    /// One or more internal DMA status bits report a failed transfer.
    ///
    /// The raw value is the exact acknowledged IDSTS snapshot; it is not
    /// translated into an unrelated controller interrupt bit.
    DmaError { raw_status: u32 },
    /// Status bits are pending but do not map to a high-level event yet.
    Other { raw_status: u32 },
}

/// Owned DWMMC IRQ top-half endpoint.
pub struct DwMmcIrq {
    irq: Arc<host::IrqCore>,
}

pub(crate) const DWMMC_INT_RESPONSE_ERROR: u32 = 1 << 1;
pub(crate) const DWMMC_INT_COMMAND_DONE: u32 = 1 << 2;
pub(crate) const DWMMC_INT_DATA_TRANSFER_OVER: u32 = 1 << 3;
pub(crate) const DWMMC_INT_TXDR: u32 = 1 << 4;
pub(crate) const DWMMC_INT_RXDR: u32 = 1 << 5;
pub(crate) const DWMMC_INT_RESPONSE_CRC_ERROR: u32 = 1 << 6;
pub(crate) const DWMMC_INT_DATA_CRC_ERROR: u32 = 1 << 7;
pub(crate) const DWMMC_INT_RESPONSE_TIMEOUT: u32 = 1 << 8;
pub(crate) const DWMMC_INT_DATA_READ_TIMEOUT: u32 = 1 << 9;
pub(crate) const DWMMC_INT_HOST_TIMEOUT: u32 = 1 << 10;
pub(crate) const DWMMC_INT_FIFO_UNDER_OVER_RUN: u32 = 1 << 11;
pub(crate) const DWMMC_INT_HARDWARE_LOCKED_WRITE: u32 = 1 << 12;
pub(crate) const DWMMC_IDMAC_INT_TI: u32 = 1 << 0;
pub(crate) const DWMMC_IDMAC_INT_RI: u32 = 1 << 1;
pub(crate) const DWMMC_IDMAC_INT_FATAL_BUS_ERROR: u32 = 1 << 2;
const DWMMC_IDMAC_INT_DESCRIPTOR_UNAVAILABLE: u32 = 1 << 4;
const DWMMC_IDMAC_INT_CARD_ERROR_SUMMARY: u32 = 1 << 5;
pub(crate) const DWMMC_IDMAC_INT_NI: u32 = 1 << 8;
pub(crate) const DWMMC_IDMAC_INT_ABNORMAL_SUMMARY: u32 = 1 << 9;
pub(crate) const DWMMC_IDMAC_INT_ERROR_MASK: u32 = DWMMC_IDMAC_INT_FATAL_BUS_ERROR
    | DWMMC_IDMAC_INT_DESCRIPTOR_UNAVAILABLE
    | DWMMC_IDMAC_INT_CARD_ERROR_SUMMARY
    | DWMMC_IDMAC_INT_ABNORMAL_SUMMARY;
pub(crate) const DWMMC_IDMAC_INT_TRANSFER_MASK: u32 = DWMMC_IDMAC_INT_TI | DWMMC_IDMAC_INT_RI;
pub(crate) const DWMMC_IDMAC_INT_ENABLE_MASK: u32 =
    DWMMC_IDMAC_INT_TRANSFER_MASK | DWMMC_IDMAC_INT_NI | DWMMC_IDMAC_INT_ERROR_MASK;
const DWMMC_IDMAC_INT_ACK_MASK: u32 = DWMMC_IDMAC_INT_ENABLE_MASK;
pub(crate) const DWMMC_INT_START_BIT_ERROR: u32 = 1 << 13;
pub(crate) const DWMMC_INT_END_BIT_ERROR: u32 = 1 << 15;
pub(crate) const DWMMC_INT_ERROR_MASK: u32 = DWMMC_INT_RESPONSE_ERROR
    | DWMMC_INT_RESPONSE_CRC_ERROR
    | DWMMC_INT_DATA_CRC_ERROR
    | DWMMC_INT_RESPONSE_TIMEOUT
    | DWMMC_INT_DATA_READ_TIMEOUT
    | DWMMC_INT_HOST_TIMEOUT
    | DWMMC_INT_FIFO_UNDER_OVER_RUN
    | DWMMC_INT_HARDWARE_LOCKED_WRITE
    | DWMMC_INT_START_BIT_ERROR
    | DWMMC_INT_END_BIT_ERROR;

pub(crate) fn event_from_raw_status(raw_status: u32) -> Event {
    let status = crate::regs::RIntSts::from_bits(raw_status);
    if raw_status == 0 {
        Event::None
    } else if status.error() {
        Event::Error { raw_status }
    } else if status.command_done() {
        Event::CommandComplete
    } else if status.data_transfer_over() {
        Event::TransferComplete
    } else if status.receive_fifo_data_request() {
        Event::ReceiveReady
    } else if status.transmit_fifo_data_request() {
        Event::TransmitReady
    } else {
        Event::Other { raw_status }
    }
}

impl HostEvent for Event {
    fn kind(&self) -> HostEventKind {
        match self {
            Event::None => HostEventKind::None,
            Event::Deferred => HostEventKind::Other,
            Event::CommandComplete => HostEventKind::CommandComplete,
            Event::TransferComplete => HostEventKind::TransferComplete,
            Event::DmaComplete => HostEventKind::Other,
            Event::ReceiveReady => HostEventKind::ReceiveReady,
            Event::TransmitReady => HostEventKind::TransmitReady,
            Event::Error { .. } | Event::DmaError { .. } => HostEventKind::Error,
            Event::Other { .. } => HostEventKind::Other,
        }
    }

    fn ack_deferred(&self) -> bool {
        matches!(self, Event::Deferred)
    }

    fn source(&self) -> HostEventSource {
        match self {
            Event::CommandComplete => HostEventSource::Command,
            Event::TransferComplete
            | Event::DmaComplete
            | Event::DmaError { .. }
            | Event::ReceiveReady
            | Event::TransmitReady => HostEventSource::Data,
            Event::None | Event::Deferred | Event::Error { .. } | Event::Other { .. } => {
                HostEventSource::Controller
            }
        }
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        match self {
            Event::TransferComplete
            | Event::DmaComplete
            | Event::DmaError { .. }
            | Event::ReceiveReady
            | Event::TransmitReady => Some(BlockRequestId::new(0)),
            Event::None
            | Event::Deferred
            | Event::CommandComplete
            | Event::Error { .. }
            | Event::Other { .. } => None,
        }
    }
}

impl DwMmc {
    pub fn irq_endpoint(&mut self) -> DwMmcIrq {
        DwMmcIrq {
            irq: self.irq.clone(),
        }
    }

    /// Read and acknowledge pending controller status, returning a stable
    /// event for OS glue to translate into wakeups or worker scheduling.
    pub fn handle_irq(&mut self) -> Event {
        handle_irq_core(&self.irq)
    }
}

impl SdioIrqHandle for DwMmcIrq {
    type Event = Event;

    fn handle_irq(&mut self) -> Self::Event {
        handle_irq_core(&self.irq)
    }
}

fn handle_irq_core(irq: &host::IrqCore) -> Event {
    let Some(_register_owner) = irq.state.try_begin_irq_snapshot() else {
        return Event::Deferred;
    };
    let generation = irq.state.generation();
    let raw_status = irq.regs.mintsts().read();
    if raw_status != 0 {
        irq.regs
            .rintsts()
            .write(crate::regs::RIntSts::from_bits(raw_status));
    }
    let fifo_ready = raw_status & (crate::DWMMC_INT_RXDR | crate::DWMMC_INT_TXDR);
    if fifo_ready != 0 {
        let mask = irq.regs.intmask().read();
        irq.regs.intmask().write(mask & !fifo_ready);
    }
    let idmac_status = irq.regs.idsts().read();
    let observed_idmac = idmac_status & DWMMC_IDMAC_INT_ACK_MASK;
    if observed_idmac != 0 {
        // Acknowledge exactly the captured causes. Writing a constant mask
        // could erase a completion that arrived after this snapshot.
        irq.regs.idsts().write(observed_idmac);
    }
    // The worker may re-enable FIFO sources as soon as it observes the
    // mailbox. Publish only after all device-side acknowledgement and masking
    // writes are globally ordered before the normal-memory event snapshot.
    mbarrier::mb();
    irq.state.cache_if_current(generation, raw_status);
    irq.state.cache_idmac_if_current(generation, observed_idmac);

    if observed_idmac & DWMMC_IDMAC_INT_ERROR_MASK != 0 {
        return Event::DmaError {
            raw_status: observed_idmac,
        };
    }
    let controller_event = event_from_raw_status(raw_status);
    if !matches!(controller_event, Event::None) {
        controller_event
    } else if observed_idmac & DWMMC_IDMAC_INT_TRANSFER_MASK != 0 {
        Event::DmaComplete
    } else if observed_idmac != 0 {
        Event::Other {
            raw_status: observed_idmac,
        }
    } else {
        Event::None
    }
}
