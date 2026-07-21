//! Owned IRQ endpoint and stable event classification.

use core::num::NonZeroU64;

use rdif_irq::{ContainmentCause, IrqCapture, IrqEndpoint, IrqSourceControl, MaskedSource};
use sdmmc_protocol::sdio::host::SdioIrqControlError;

use crate::*;

pub(crate) fn event_from_status(normal: u16, error: u16) -> Event {
    Event::from_status(normal, error)
}

impl HostEvent for Event {
    fn kind(&self) -> HostEventKind {
        if self.normal & NORMAL_INT_ERROR != 0 || self.error != 0 {
            HostEventKind::Error
        } else if self.normal & NORMAL_INT_XFER_COMPLETE != 0 {
            HostEventKind::TransferComplete
        } else if self.normal & NORMAL_INT_BUFFER_READ_READY != 0 {
            HostEventKind::ReceiveReady
        } else if self.normal & NORMAL_INT_BUFFER_WRITE_READY != 0 {
            HostEventKind::TransmitReady
        } else if self.normal & NORMAL_INT_CMD_COMPLETE != 0 {
            HostEventKind::CommandComplete
        } else if !self.is_empty() {
            HostEventKind::Other
        } else {
            HostEventKind::None
        }
    }

    fn source(&self) -> HostEventSource {
        match self.kind() {
            HostEventKind::CommandComplete => HostEventSource::Command,
            HostEventKind::TransferComplete
            | HostEventKind::ReceiveReady
            | HostEventKind::TransmitReady => HostEventSource::Data,
            _ => HostEventSource::Controller,
        }
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        match self.kind() {
            HostEventKind::TransferComplete
            | HostEventKind::ReceiveReady
            | HostEventKind::TransmitReady => Some(BlockRequestId::new(0)),
            _ => None,
        }
    }

    fn requests_block_queue_service(&self) -> bool {
        self.normal & NORMAL_INT_REQUEST_MASK != 0 || self.error != 0
    }

    fn stable_summary(&self) -> HostEventSummary {
        HostEventSummary {
            stable_status: u32::from(self.normal) | (u32::from(self.error) << 16),
            dma_status: 0,
            queue_service: self.requests_block_queue_service(),
            card_function_interrupt: self.normal & NORMAL_INT_CARD_INTERRUPT != 0,
        }
    }
}

impl Sdhci {
    pub fn block_buffer_config(&self, mode: BlockTransferMode) -> BlockBufferConfig {
        match mode {
            BlockTransferMode::Fifo => {
                BlockBufferConfig::new(NonZeroUsize::new(512).unwrap(), 1, None)
            }
            BlockTransferMode::Dma => {
                BlockBufferConfig::new(NonZeroUsize::new(512).unwrap(), 512, Some(self.dma_mask))
            }
            // Future BlockTransferMode variants fall back to the conservative Fifo config.
            _ => BlockBufferConfig::new(NonZeroUsize::new(512).unwrap(), 1, None),
        }
    }

    /// Acquires this controller's unique live split IRQ-source lease.
    ///
    /// The caller must move the capture endpoint into an IRQ action registered
    /// by the same CPU-pinned maintenance thread that retains the control
    /// endpoint and this [`Sdhci`]. Controller delivery must remain disabled
    /// until that registration succeeds. A later activation may acquire a new
    /// lease only after both halves of the synchronized old lease are retired.
    pub fn take_irq_source(&mut self) -> Option<SdhciIrqSource> {
        let source = self.take_irq_source_for(IrqPublication::LegacyMailbox);
        if source.is_some() {
            self.evidence_irq = false;
        }
        source
    }

    /// Acquires the source whose endpoint publishes only typed v0.13 ledger
    /// evidence and never fills the compatibility task mailbox.
    pub fn take_evidence_irq_source(&mut self) -> Option<SdhciIrqSource> {
        let source = self.take_irq_source_for(IrqPublication::EvidenceLedger);
        if source.is_some() {
            self.evidence_irq = true;
        }
        source
    }

    fn take_irq_source_for(&mut self, publication: IrqPublication) -> Option<SdhciIrqSource> {
        self.irq.state.take_source().then(|| {
            SdioIrqSource::new(
                SdhciIrqEndpoint {
                    irq: Arc::clone(&self.irq),
                    publication,
                },
                SdhciIrqControl {
                    irq: Arc::clone(&self.irq),
                },
            )
        })
    }
}

impl IrqEndpoint for SdhciIrqEndpoint {
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
        self.irq.state.mark_source_masked();
        Ok(MaskedSource::new(
            generation,
            NonZeroU64::new(host::SDHCI_IRQ_SOURCE_BITMAP).expect("SDHCI source bitmap is nonzero"),
        ))
    }
}

impl Drop for SdhciIrqEndpoint {
    fn drop(&mut self) {
        // Registration teardown must already have masked and synchronized the
        // action. Drop only retires the capability; it never fabricates that
        // hardware protocol or implicitly rearms the source.
        self.irq.state.release_capture_endpoint();
    }
}

impl IrqSourceControl for SdhciIrqControl {
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
        let bitmap = source.bitmap().get();
        if bitmap != host::SDHCI_IRQ_SOURCE_BITMAP {
            return Err(SdioIrqControlError::SourceNotMasked { bitmap });
        }
        if !self.irq.state.source_online() {
            return Err(SdioIrqControlError::Offline);
        }
        if !self.irq.state.claim_masked_source(bitmap) {
            return Err(SdioIrqControlError::SourceNotMasked { bitmap });
        }
        write_irq_signal_enable(
            &self.irq,
            NORMAL_INT_COMPLETION_SIGNAL_MASK,
            ERROR_INT_COMPLETION_SIGNAL_MASK,
        );
        self.irq.state.set_delivery_enabled(true);
        Ok(())
    }
}

impl Drop for SdhciIrqControl {
    fn drop(&mut self) {
        self.irq.state.release_source_control();
    }
}

fn capture_irq_core(irq: &host::IrqCore, publication: IrqPublication) -> IrqCapture<Event, Error> {
    let generation = irq.state.generation();
    let (normal, error) = if irq.aligned_32bit {
        let status = read_u32(irq.base_addr, REG_NORMAL_INT_STATUS);
        let normal = status as u16;
        let error = if normal & NORMAL_INT_ERROR != 0 {
            (status >> 16) as u16
        } else {
            0
        };
        (normal, error)
    } else {
        let normal = read_u16(irq.base_addr, REG_NORMAL_INT_STATUS);
        let error = if normal & NORMAL_INT_ERROR != 0 {
            read_u16(irq.base_addr, REG_ERROR_INT_STATUS)
        } else {
            0
        };
        (normal, error)
    };

    if irq.aligned_32bit {
        if normal != 0 || error != 0 {
            write_u32(
                irq.base_addr,
                REG_NORMAL_INT_STATUS,
                u32::from(normal) | (u32::from(error) << 16),
            );
        }
    } else {
        if normal != 0 {
            write_u16(irq.base_addr, REG_NORMAL_INT_STATUS, normal);
        }
        if error != 0 {
            write_u16(irq.base_addr, REG_ERROR_INT_STATUS, error);
        }
    }
    // Card-detect, SDIO-card, re-tuning, and vendor sideband causes are
    // controller-owned. They are acknowledged here but must never become
    // request-generation evidence or prevent the next command handoff.
    if matches!(publication, IrqPublication::LegacyMailbox) {
        irq.state
            .cache_if_current(generation, normal & NORMAL_INT_REQUEST_MASK, error);
    }

    let event = event_from_status(normal, error);
    if event.is_empty() {
        IrqCapture::Unhandled
    } else {
        IrqCapture::Captured {
            event,
            masked: None,
        }
    }
}

fn mask_irq_delivery(irq: &host::IrqCore) {
    write_irq_signal_enable(irq, 0, 0);
    irq.state.set_delivery_enabled(false);
}

fn write_irq_signal_enable(irq: &host::IrqCore, normal: u16, error: u16) {
    if irq.aligned_32bit {
        write_u32(
            irq.base_addr,
            REG_NORMAL_INT_SIGNAL_ENABLE,
            u32::from(normal) | (u32::from(error) << 16),
        );
    } else {
        write_u16(irq.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE, normal);
        write_u16(irq.base_addr, REG_ERROR_INT_SIGNAL_ENABLE, error);
    }
}

fn read_u32(base_addr: usize, off: usize) -> u32 {
    unsafe { core::ptr::read_volatile((base_addr + off) as *const u32) }
}

fn write_u32(base_addr: usize, off: usize, val: u32) {
    unsafe { core::ptr::write_volatile((base_addr + off) as *mut u32, val) }
}

fn read_u16(base_addr: usize, off: usize) -> u16 {
    unsafe { core::ptr::read_volatile((base_addr + off) as *const u16) }
}

fn write_u16(base_addr: usize, off: usize, val: u16) {
    unsafe { core::ptr::write_volatile((base_addr + off) as *mut u16, val) }
}
