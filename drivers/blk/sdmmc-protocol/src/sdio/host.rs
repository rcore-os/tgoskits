//! SD/MMC IRQ capability layered on the portable `sdmmc-host` bus contract.

use core::{num::NonZeroU16, time::Duration};

use dma_api::DeviceDma;
pub use sdmmc_host::{BusWidth, ClockSpeed, SignalVoltage};

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
    CardInterrupt,
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

    /// Return whether the snapshot contains an SDIO `CARD_INT` source.
    ///
    /// A controller may report this together with command/data completion;
    /// implementations must preserve both facts in that case.
    fn card_interrupt(&self) -> bool {
        matches!(self.kind(), HostEventKind::CardInterrupt)
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
pub trait SdMmcIrqHandle: Send + 'static {
    type Event: HostEvent + Default;

    fn handle_irq(&mut self) -> Self::Event;
}

/// Task-context mask/rearm endpoint for the SDIO card interrupt source.
pub trait CardIrqControl: Send + 'static {
    /// Mask the level-sensitive card-interrupt source in both controller
    /// ownership and parent-IRQ delivery masks.
    ///
    /// SDHCI exposes these as separate `INT_ENABLE` and `SIGNAL_ENABLE`
    /// registers, but Linux keeps one `ier` mirror and clears the bit in both
    /// registers when the top half observes `CARD_INT`.  Keeping the two
    /// masks in lockstep prevents a level source from re-entering the owner
    /// while its drain operation is still in progress.
    fn mask(&mut self);

    /// Disable the card-interrupt signal for shutdown.
    fn disable(&mut self);

    /// Unmask the signal and close the drain/rearm race with a status
    /// readback. Returns `true` when the source was already asserted and has
    /// therefore been masked again.
    fn rearm_and_check(&mut self) -> bool;
}

impl CardIrqControl for () {
    fn mask(&mut self) {}

    fn disable(&mut self) {}

    fn rearm_and_check(&mut self) -> bool {
        false
    }
}

/// Source required before the next protocol progress step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostProgressWait {
    Irq,
    Register { retry_after: Duration },
}

/// Result of closing the completion-IRQ drain/rearm window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CompletionIrqRearm {
    /// No completion status was latched when delivery was restored.
    Idle,
    /// A completion was already latched and was published to the host's
    /// task-context completion mailbox.
    Pending,
}

/// IRQ and DMA capabilities required by the SD/MMC protocol runtime.
///
/// Command, data, and bus transactions are provided directly by
/// [`sdmmc_host::SdMmcHost`]; this trait intentionally does not duplicate them.
pub trait SdMmcIrqHost: sdmmc_host::SdMmcHost {
    type Event: HostEvent + Default;
    type IrqHandle: SdMmcIrqHandle<Event = Self::Event>;
    type CardIrq: CardIrqControl;

    /// Consume the host into independently owned bus, hard-IRQ, and card-IRQ
    /// endpoints.
    fn into_parts(self) -> sdmmc_host::HostParts<Self, Self::IrqHandle, Self::CardIrq>
    where
        Self: Sized;

    fn completion_irq_enabled(&self) -> bool {
        false
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Restore completion-IRQ delivery and synchronously capture status that
    /// became pending while delivery was masked.
    ///
    /// A host returning [`CompletionIrqRearm::Pending`] must have published the
    /// captured status through the same mailbox consumed by an
    /// `AcknowledgedIrq` progress step. This closes the edge-triggered parent
    /// IRQ race without moving protocol progress into the IRQ top half.
    fn rearm_completion_irq_and_check(&mut self) -> Result<CompletionIrqRearm, Error>;

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
        HostEventKind::None | HostEventKind::CardInterrupt => None,
        _ => Some(SDMMC_BLOCK_QUEUE_ID),
    }
}

/// Protocol-level naming for portable host bus operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdMmcBusOp {
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

impl SdMmcBusOp {
    pub(super) fn into_host_op(self) -> sdmmc_host::BusOp {
        match self {
            Self::ResetAll => sdmmc_host::BusOp::ResetAll,
            Self::PowerOn => sdmmc_host::BusOp::PowerOn,
            Self::PowerOff => sdmmc_host::BusOp::PowerOff,
            Self::SetBusWidth(width) => sdmmc_host::BusOp::SetBusWidth(width),
            Self::SetClock(speed) => sdmmc_host::BusOp::SetClock(speed),
            Self::SwitchVoltage(voltage) => sdmmc_host::BusOp::SetSignalVoltage(voltage),
            Self::ExecuteTuning {
                cmd_index,
                block_size,
            } => sdmmc_host::BusOp::ExecuteTuning {
                command: Command::new(cmd_index, 0, crate::response::ResponseType::R1),
                block_size,
            },
        }
    }
}
