//! IRQ-owned destructive status acknowledgement and stable event snapshots.

use core::num::NonZeroU64;

use rdif_irq::{ContainmentCause, IrqCapture, IrqEndpoint, IrqSourceControl, MaskedSource};
use sdmmc_protocol::sdio::host::SdioIrqControlError;

use super::*;

/// Stable controller event extracted from DW_mshc raw interrupt status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
    #[default]
    None,
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
    /// Complete controller and IDMAC banks acknowledged by one hard-IRQ
    /// capture. Evidence-mode users consume this exact pair linearly.
    Snapshot { raw_status: u32, idmac_status: u32 },
}

/// Hard-IRQ-owned destructive status capture endpoint.
pub struct DwMmcIrqEndpoint {
    irq: Arc<host::IrqCore>,
    publication: IrqPublication,
}

#[derive(Clone, Copy)]
enum IrqPublication {
    LegacyMailbox,
    EvidenceLedger,
}

/// Maintenance-owner capability for generation-checked source rearming.
pub struct DwMmcIrqControl {
    irq: Arc<host::IrqCore>,
}

/// Split ownership of this controller's unique runtime interrupt source.
pub type DwMmcIrqSource = SdioIrqSource<DwMmcIrqEndpoint, DwMmcIrqControl>;

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
            Event::CommandComplete => HostEventKind::CommandComplete,
            Event::TransferComplete => HostEventKind::TransferComplete,
            Event::DmaComplete => HostEventKind::Other,
            Event::ReceiveReady => HostEventKind::ReceiveReady,
            Event::TransmitReady => HostEventKind::TransmitReady,
            Event::Error { .. } | Event::DmaError { .. } => HostEventKind::Error,
            Event::Other { .. } => HostEventKind::Other,
            Event::Snapshot {
                raw_status,
                idmac_status,
            } => classify_captured(*raw_status, *idmac_status),
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
            Event::Snapshot { .. } if matches!(self.kind(), HostEventKind::CommandComplete) => {
                HostEventSource::Command
            }
            Event::Snapshot { .. }
                if matches!(
                    self.kind(),
                    HostEventKind::TransferComplete
                        | HostEventKind::ReceiveReady
                        | HostEventKind::TransmitReady
                ) =>
            {
                HostEventSource::Data
            }
            Event::None | Event::Error { .. } | Event::Other { .. } | Event::Snapshot { .. } => {
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
            Event::Snapshot { .. }
                if matches!(
                    self.kind(),
                    HostEventKind::TransferComplete
                        | HostEventKind::ReceiveReady
                        | HostEventKind::TransmitReady
                ) =>
            {
                Some(BlockRequestId::new(0))
            }
            Event::None
            | Event::CommandComplete
            | Event::Error { .. }
            | Event::Other { .. }
            | Event::Snapshot { .. } => None,
        }
    }

    fn requests_block_queue_service(&self) -> bool {
        match self {
            Event::Snapshot {
                raw_status,
                idmac_status,
            } => request_status(*raw_status, *idmac_status) != 0,
            Event::None => false,
            _ => true,
        }
    }

    fn stable_summary(&self) -> HostEventSummary {
        let (stable_status, dma_status) = match self {
            Event::Snapshot {
                raw_status,
                idmac_status,
            } => (*raw_status, *idmac_status),
            Event::Error { raw_status } | Event::Other { raw_status } => (*raw_status, 0),
            Event::DmaError { raw_status } => (0, *raw_status),
            Event::CommandComplete => (DWMMC_INT_COMMAND_DONE, 0),
            Event::TransferComplete => (DWMMC_INT_DATA_TRANSFER_OVER, 0),
            Event::DmaComplete => (0, DWMMC_IDMAC_INT_TRANSFER_MASK),
            Event::ReceiveReady => (DWMMC_INT_RXDR, 0),
            Event::TransmitReady => (DWMMC_INT_TXDR, 0),
            Event::None => (0, 0),
        };
        HostEventSummary {
            stable_status,
            dma_status,
            queue_service: self.requests_block_queue_service(),
            card_function_interrupt: false,
        }
    }
}

fn classify_captured(raw_status: u32, idmac_status: u32) -> HostEventKind {
    let controller = event_from_raw_status(raw_status);
    if matches!(controller, Event::Error { .. }) || idmac_status & DWMMC_IDMAC_INT_ERROR_MASK != 0 {
        HostEventKind::Error
    } else if matches!(controller, Event::TransferComplete)
        || idmac_status & DWMMC_IDMAC_INT_TRANSFER_MASK != 0
    {
        HostEventKind::TransferComplete
    } else if matches!(controller, Event::ReceiveReady) {
        HostEventKind::ReceiveReady
    } else if matches!(controller, Event::TransmitReady) {
        HostEventKind::TransmitReady
    } else if matches!(controller, Event::CommandComplete) {
        HostEventKind::CommandComplete
    } else if raw_status != 0 || idmac_status != 0 {
        HostEventKind::Other
    } else {
        HostEventKind::None
    }
}

fn request_status(raw_status: u32, idmac_status: u32) -> u32 {
    raw_status
        & (DWMMC_INT_COMMAND_DONE
            | DWMMC_INT_DATA_TRANSFER_OVER
            | DWMMC_INT_TXDR
            | DWMMC_INT_RXDR
            | DWMMC_INT_ERROR_MASK)
        | idmac_status & (DWMMC_IDMAC_INT_TRANSFER_MASK | DWMMC_IDMAC_INT_ERROR_MASK)
}

impl DwMmc {
    /// Acquires this controller's unique live split IRQ-source lease.
    ///
    /// The caller must move the capture endpoint into an IRQ action registered
    /// by the same CPU-pinned maintenance thread that retains this host and
    /// the control endpoint. Delivery must remain disabled until registration
    /// succeeds. A later activation may acquire a new lease only after both
    /// halves of the synchronized old lease have retired.
    pub fn take_irq_source(&mut self) -> Option<DwMmcIrqSource> {
        let source = self.take_irq_source_for(IrqPublication::LegacyMailbox);
        if source.is_some() {
            self.evidence_irq = false;
        }
        source
    }

    /// Acquires the source whose endpoint publishes only complete v0.13
    /// ledger snapshots.
    pub fn take_evidence_irq_source(&mut self) -> Option<DwMmcIrqSource> {
        let source = self.take_irq_source_for(IrqPublication::EvidenceLedger);
        if source.is_some() {
            self.evidence_irq = true;
        }
        source
    }

    fn take_irq_source_for(&mut self, publication: IrqPublication) -> Option<DwMmcIrqSource> {
        self.irq.state.take_source().then(|| {
            SdioIrqSource::new(
                DwMmcIrqEndpoint {
                    irq: Arc::clone(&self.irq),
                    publication,
                },
                DwMmcIrqControl {
                    irq: Arc::clone(&self.irq),
                },
            )
        })
    }
}

impl IrqEndpoint for DwMmcIrqEndpoint {
    type Event = Event;
    type Fault = Error;

    fn capture(&mut self) -> IrqCapture<Self::Event, Self::Fault> {
        capture_irq_core(&self.irq, self.publication)
    }

    fn contain(&mut self, _cause: ContainmentCause) -> Result<MaskedSource, Self::Fault> {
        mask_irq_delivery(&self.irq);
        let generation = self
            .irq
            .state
            .source_generation()
            .ok_or(Error::InvalidArgument)?;
        let bitmap = u64::from(self.irq.state.desired_sources());
        let bitmap = NonZeroU64::new(bitmap).ok_or(Error::InvalidArgument)?;
        self.irq.state.mark_sources_masked(bitmap.get());
        Ok(MaskedSource::new(generation, bitmap))
    }
}

impl Drop for DwMmcIrqEndpoint {
    fn drop(&mut self) {
        // Registration teardown must already have masked and synchronized the
        // action. Drop only retires the capability; it never fabricates that
        // hardware protocol or implicitly rearms the source.
        self.irq.state.release_capture_endpoint();
    }
}

impl IrqSourceControl for DwMmcIrqControl {
    type Error = SdioIrqControlError;

    fn rearm(&mut self, source: MaskedSource) -> Result<(), Self::Error> {
        let expected = self
            .irq
            .state
            .source_generation()
            .ok_or(SdioIrqControlError::Offline)?;
        let actual = source.generation();
        if actual != expected {
            return Err(SdioIrqControlError::StaleGeneration {
                expected: expected.get(),
                actual: actual.get(),
            });
        }
        if !self.irq.state.source_online() {
            return Err(SdioIrqControlError::Offline);
        }
        let bitmap = source.bitmap().get();
        let Ok(controller_sources) = u32::try_from(bitmap) else {
            return Err(SdioIrqControlError::SourceNotMasked { bitmap });
        };
        if controller_sources & !self.irq.state.desired_sources() != 0
            || !self.irq.state.claim_masked_sources(bitmap)
        {
            return Err(SdioIrqControlError::SourceNotMasked { bitmap });
        }

        // The maintenance owner invokes rearm with this local action excluded,
        // so no endpoint RMW can race this register transition.
        let current = self.irq.regs.intmask().read();
        self.irq.regs.intmask().write(current | controller_sources);
        self.irq
            .regs
            .ctrl()
            .update(|control| control.with_int_enable(true));
        Ok(())
    }
}

impl Drop for DwMmcIrqControl {
    fn drop(&mut self) {
        self.irq.state.release_source_control();
    }
}

fn capture_irq_core(irq: &host::IrqCore, publication: IrqPublication) -> IrqCapture<Event, Error> {
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
        irq.state.mark_sources_masked(u64::from(fifo_ready));
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
    if matches!(publication, IrqPublication::LegacyMailbox) {
        irq.state.cache_if_current(generation, raw_status);
        irq.state.cache_idmac_if_current(generation, observed_idmac);
    }

    let event = if matches!(publication, IrqPublication::EvidenceLedger)
        && (raw_status != 0 || observed_idmac != 0)
    {
        Event::Snapshot {
            raw_status,
            idmac_status: observed_idmac,
        }
    } else if observed_idmac & DWMMC_IDMAC_INT_ERROR_MASK != 0 {
        Event::DmaError {
            raw_status: observed_idmac,
        }
    } else {
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
    };
    if matches!(event, Event::None) {
        return IrqCapture::Unhandled;
    }

    let masked = NonZeroU64::new(u64::from(fifo_ready)).and_then(|bitmap| {
        irq.state
            .source_generation()
            .map(|generation| MaskedSource::new(generation, bitmap))
    });
    IrqCapture::Captured { event, masked }
}

fn mask_irq_delivery(irq: &host::IrqCore) {
    irq.regs.intmask().write(0);
    irq.regs
        .ctrl()
        .update(|control| control.with_int_enable(false));
}
