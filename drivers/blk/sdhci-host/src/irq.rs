//! Owned IRQ endpoint and stable event classification.

use crate::*;

pub(crate) fn event_from_status(normal: u16, error: u16) -> Event {
    if normal & NORMAL_INT_ERROR != 0 || error != 0 {
        Event::Error { normal, error }
    } else if normal & NORMAL_INT_XFER_COMPLETE != 0 {
        Event::TransferComplete
    } else if normal & NORMAL_INT_BUFFER_READ_READY != 0 {
        Event::ReceiveReady
    } else if normal & NORMAL_INT_BUFFER_WRITE_READY != 0 {
        Event::TransmitReady
    } else if normal & NORMAL_INT_CMD_COMPLETE != 0 {
        Event::CommandComplete
    } else if normal != 0 || error != 0 {
        Event::Other { normal, error }
    } else {
        Event::None
    }
}

impl HostEvent for Event {
    fn kind(&self) -> HostEventKind {
        match self {
            Event::None => HostEventKind::None,
            Event::CommandComplete => HostEventKind::CommandComplete,
            Event::TransferComplete => HostEventKind::TransferComplete,
            Event::ReceiveReady => HostEventKind::ReceiveReady,
            Event::TransmitReady => HostEventKind::TransmitReady,
            Event::Error { .. } => HostEventKind::Error,
            Event::Other { .. } => HostEventKind::Other,
        }
    }

    fn source(&self) -> HostEventSource {
        match self {
            Event::CommandComplete => HostEventSource::Command,
            Event::TransferComplete | Event::ReceiveReady | Event::TransmitReady => {
                HostEventSource::Data
            }
            Event::None | Event::Error { .. } | Event::Other { .. } => HostEventSource::Controller,
        }
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        match self {
            Event::TransferComplete | Event::ReceiveReady | Event::TransmitReady => {
                Some(BlockRequestId::new(0))
            }
            Event::None | Event::CommandComplete | Event::Error { .. } | Event::Other { .. } => {
                None
            }
        }
    }

    fn requests_block_queue_service(&self) -> bool {
        match self {
            Event::None => false,
            Event::Other { normal, error } => normal & NORMAL_INT_REQUEST_MASK != 0 || *error != 0,
            Event::CommandComplete
            | Event::TransferComplete
            | Event::ReceiveReady
            | Event::TransmitReady
            | Event::Error { .. } => true,
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

    pub fn irq_endpoint(&mut self) -> SdhciIrqHandle {
        SdhciIrqHandle {
            irq: self.irq.clone(),
        }
    }
}

impl SdioIrqHandle for SdhciIrqHandle {
    type Event = Event;

    fn handle_irq(&mut self) -> Self::Event {
        handle_irq_core(&self.irq)
    }
}

fn handle_irq_core(irq: &host::IrqCore) -> Event {
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
    irq.state
        .cache_if_current(generation, normal & NORMAL_INT_REQUEST_MASK, error);

    event_from_status(normal, error)
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
