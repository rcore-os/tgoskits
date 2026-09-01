use super::*;

/// Owned DWMMC IRQ top-half endpoint.
pub struct DwMmcIrq {
    irq: Arc<host::IrqCore>,
}

pub(crate) const DWMMC_INT_RESPONSE_ERROR: u32 = 1 << 1;
pub(crate) const DWMMC_INT_COMMAND_DONE: u32 = 1 << 2;
pub(crate) const DWMMC_INT_DATA_TRANSFER_OVER: u32 = 1 << 3;
pub(crate) const DWMMC_INT_RESPONSE_CRC_ERROR: u32 = 1 << 6;
pub(crate) const DWMMC_INT_DATA_CRC_ERROR: u32 = 1 << 7;
pub(crate) const DWMMC_INT_RESPONSE_TIMEOUT: u32 = 1 << 8;
pub(crate) const DWMMC_INT_DATA_READ_TIMEOUT: u32 = 1 << 9;
pub(crate) const DWMMC_INT_HOST_TIMEOUT: u32 = 1 << 10;
pub(crate) const DWMMC_INT_FIFO_UNDER_OVER_RUN: u32 = 1 << 11;
pub(crate) const DWMMC_INT_HARDWARE_LOCKED_WRITE: u32 = 1 << 12;
pub(crate) const DWMMC_LATCH_IDMAC_COMPLETE: u32 = 1 << 30;
pub(crate) const DWMMC_LATCH_IDMAC_ERROR: u32 = 1 << 31;
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

impl SdMmcIrqHost for DwMmc {
    type Event = Event;
    type IrqHandle = DwMmcIrq;
    type CardIrq = ();

    fn into_parts(mut self) -> sdmmc_host::HostParts<Self, Self::IrqHandle, Self::CardIrq> {
        let irq = DwMmc::irq_endpoint(&mut self);
        sdmmc_host::HostParts {
            bus: self,
            irq,
            card_irq: None,
        }
    }

    fn completion_irq_enabled(&self) -> bool {
        DwMmc::completion_irq_enabled(self)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        DwMmc::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        DwMmc::disable_completion_irq(self);
        Ok(())
    }

    fn device_dma(&self) -> Result<&dma_api::DeviceDma, Error> {
        self.dma.as_ref().ok_or(Error::UnsupportedCommand)
    }

    fn progress_wait_kind(&self) -> sdmmc_protocol::sdio::HostProgressWait {
        if self.command_needs_register_retry() {
            sdmmc_protocol::sdio::HostProgressWait::Register {
                retry_after: DWMMC_REGISTER_RETRY_DELAY,
            }
        } else {
            sdmmc_protocol::sdio::HostProgressWait::Irq
        }
    }
}

impl HostEvent for Event {
    fn kind(&self) -> HostEventKind {
        match self {
            Event::None => HostEventKind::None,
            Event::CommandComplete => HostEventKind::CommandComplete,
            Event::TransferComplete => HostEventKind::TransferComplete,
            Event::Error { .. } => HostEventKind::Error,
            Event::Other { .. } => HostEventKind::Other,
        }
    }

    fn source(&self) -> HostEventSource {
        match self {
            Event::CommandComplete => HostEventSource::Command,
            Event::TransferComplete => HostEventSource::Data,
            Event::None | Event::Error { .. } | Event::Other { .. } => HostEventSource::Controller,
        }
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        match self {
            Event::TransferComplete => Some(BlockRequestId::new(0)),
            Event::None | Event::CommandComplete | Event::Error { .. } | Event::Other { .. } => {
                None
            }
        }
    }
}

impl DwMmc {
    pub(crate) fn irq_endpoint(&mut self) -> DwMmcIrq {
        DwMmcIrq {
            irq: self.irq.clone(),
        }
    }
}

impl SdMmcIrqHandle for DwMmcIrq {
    type Event = Event;

    fn handle_irq(&mut self) -> Self::Event {
        handle_irq_core(&self.irq)
    }
}

fn handle_irq_core(irq: &host::IrqCore) -> Event {
    let generation = irq.state.generation();
    let masked_status = irq.regs.mintsts().read();
    if masked_status != 0 {
        irq.regs
            .rintsts()
            .write(crate::regs::RIntSts::from_bits(masked_status));
    }
    let mut enabled_status = masked_status & irq.regs.intmask().read();
    let idmac_status = irq.regs.idsts().read();
    let idmac_ack = idmac_status & dma::IDMAC_INT_CLR;
    if idmac_ack != 0 {
        irq.regs.idsts().write(idmac_ack);
    }
    let enabled_idmac_status = idmac_status & irq.regs.idinten().read();
    if enabled_idmac_status & (dma::IDMAC_INT_TI | dma::IDMAC_INT_RI) != 0 {
        enabled_status |= DWMMC_LATCH_IDMAC_COMPLETE;
    }
    if enabled_idmac_status & dma::IDMAC_INT_ERROR != 0 {
        enabled_status |= DWMMC_LATCH_IDMAC_ERROR;
    }
    irq.state.cache_if_current(generation, enabled_status);
    event_from_raw_status(enabled_status)
}

pub(crate) fn event_from_raw_status(raw_status: u32) -> Event {
    if raw_status & DWMMC_LATCH_IDMAC_ERROR != 0 {
        return Event::Error { raw_status };
    }
    let status = crate::regs::RIntSts::from_bits(raw_status);
    if raw_status == 0 {
        Event::None
    } else if status.error() {
        Event::Error { raw_status }
    } else if status.command_done() {
        Event::CommandComplete
    } else if status.data_transfer_over() {
        Event::TransferComplete
    } else {
        Event::Other { raw_status }
    }
}
