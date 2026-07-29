//! SD/MMC IRQ capability layered on the portable `sdio-host2` bus contract.

use core::{num::NonZeroU16, time::Duration};

use dma_api::DeviceDma;
pub use sdio_host2::{BusWidth, ClockSpeed, SignalVoltage};

use crate::{block::BlockRequestId, cmd::Command, error::Error};

/// Host IRQ event category returned by portable controller cores.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostEventKind {
    #[default]
    None,
    CommandComplete,
    TransferComplete,
    ReceiveReady,
    TransmitReady,
    Error,
    Other,
}

/// Hardware engine affected by a host IRQ event.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostEventSource {
    #[default]
    Controller,
    Command,
    Data,
}

/// Stable event summary extracted by a host-controller IRQ handler.
pub trait HostEvent {
    fn kind(&self) -> HostEventKind;

    fn source(&self) -> HostEventSource {
        HostEventSource::Controller
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        None
    }
}

impl HostEvent for () {
    fn kind(&self) -> HostEventKind {
        HostEventKind::None
    }
}

/// Move-only hard-IRQ acknowledgement endpoint owned by OS IRQ registration.
///
/// `handle_irq` may only read/ack status and cache a compact event. It must not
/// touch DMA ownership, advance protocol state, complete requests, or call task
/// APIs.
pub trait SdioIrqHandle: Send + 'static {
    type Event: HostEvent + Default;

    fn handle_irq(&mut self) -> Self::Event;
}

/// Source required before the next protocol progress step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostProgressWait {
    Irq,
    Register { retry_after: Duration },
}

/// IRQ and DMA capabilities required by the SD/MMC protocol runtime.
///
/// Command, data, and bus transactions are provided directly by
/// [`sdio_host2::SdioHost`]; this trait intentionally does not duplicate them.
pub trait SdioIrqHost: sdio_host2::SdioHost {
    type Event: HostEvent + Default;
    type IrqHandle: SdioIrqHandle<Event = Self::Event>;

    fn irq_handle(&mut self) -> Self::IrqHandle;

    fn completion_irq_enabled(&self) -> bool {
        false
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Returns the DMA capability owned by this physical host.
    ///
    /// Protocol initialization uses it for CPU-owned scratch DMA. Production
    /// block I/O already arrives as `PreparedDma`.
    fn device_dma(&self) -> Result<&DeviceDma, Error>;

    fn progress_wait_kind(&self) -> HostProgressWait {
        HostProgressWait::Irq
    }
}

/// Queue identifier used by single-queue SD/MMC block adapters.
pub const SDMMC_BLOCK_QUEUE_ID: usize = 0;

pub fn block_queue_ready_from_host_event(event: &impl HostEvent) -> Option<usize> {
    match event.kind() {
        HostEventKind::None => None,
        _ => Some(SDMMC_BLOCK_QUEUE_ID),
    }
}

/// Protocol-level naming for portable host bus operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdioBusOp {
    ResetAll,
    PowerOn,
    PowerOff,
    SetBusWidth(BusWidth),
    SetClock(ClockSpeed),
    SwitchVoltage(SignalVoltage),
    ExecuteTuning {
        cmd_index: u8,
        block_size: NonZeroU16,
    },
}

impl SdioBusOp {
    pub(super) fn into_host_op(self) -> sdio_host2::BusOp {
        match self {
            Self::ResetAll => sdio_host2::BusOp::ResetAll,
            Self::PowerOn => sdio_host2::BusOp::PowerOn,
            Self::PowerOff => sdio_host2::BusOp::PowerOff,
            Self::SetBusWidth(width) => sdio_host2::BusOp::SetBusWidth(width),
            Self::SetClock(speed) => sdio_host2::BusOp::SetClock(speed),
            Self::SwitchVoltage(voltage) => sdio_host2::BusOp::SetSignalVoltage(voltage),
            Self::ExecuteTuning {
                cmd_index,
                block_size,
            } => sdio_host2::BusOp::ExecuteTuning {
                command: Command::new(cmd_index, 0, crate::response::ResponseType::R1),
                block_size,
            },
        }
    }
}
