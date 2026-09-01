//! SDHCI host controller backend for the `sdmmc-protocol` driver crate.
//!
//! This crate ports the [SD Host Controller Standard Specification][sdhci]
//! v3.x register layout and ADMA2 data path into a physical
//! [`sdmmc_host::SdMmcHost`] implementation that
//! [`sdmmc_protocol::sdio::native::SdMmcCard`] drives through
//! [`sdmmc_protocol::sdio::native::SdMmcCard::new`].
//!
//! # Scope
//!
//! - **Implemented**: **ADMA2 (32-bit) transfers**, 1-bit /
//!   4-bit / 8-bit bus, default-speed and high-speed clocking, 32-bit response
//!   slots, 136-bit R2 reconstruction, software reset / clock setup.
//! - **Out of scope (for now)**: 64-bit ADMA2, HS200 / SDR50 / SDR104
//!   clocking, and tuning (CMD19 / CMD21). Protocol data commands, including
//!   eMMC `SEND_EXT_CSD`, use the same ADMA2 path as normal block I/O. 1.8 V
//!   signaling is wired up at the register level but is gated behind
//!   [`Sdhci::enable_1v8_signaling`] — platforms that haven't plumbed the
//!   IO-rail regulator MUST leave it off so the protocol layer falls back
//!   instead of corrupting transfers.
//!
//! # Usage
//!
//! ```no_run
//! use core::ptr::NonNull;
//!
//! use dma_api::DeviceDma;
//! use sdhci_host::Sdhci;
//! use sdmmc_protocol::sdio::native::SdMmcCard;
//!
//! let mmio = NonNull::new(0xFE31_0000 as *mut u8).unwrap();
//! let dma: DeviceDma = todo!("install the platform DMA capability");
//! let mut host = unsafe { Sdhci::new(mmio) };
//! host.configure_dma(dma)?;
//! let mut card = SdMmcCard::new(host);
//! let mut request = card.submit_init()?;
//! // Advance `request` only from the runtime's IRQ or bounded-deadline events.
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! Construction is `unsafe` because the caller must guarantee that the
//! supplied address is a valid, exclusively-owned SDHCI register file.
//!
//! [sdhci]: https://www.sdcard.org/downloads/pls/

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use alloc::sync::Arc;
use core::{
    marker::PhantomData,
    ptr::NonNull,
    sync::atomic::{Ordering, fence},
    time::Duration,
};

mod block_path;
mod command;
mod dma;
mod host;
mod host2;
pub mod rdif;
mod regs;

pub use dma::{
    ADMA2_DESC_ALIGN, ADMA2_DESC_COUNT, ADMA2_MAX_BLOCKS, ADMA2_MAX_TRANSFER_SIZE,
    DWC_MSHC_ADMA_BOUNDARY,
};
pub use host::{HostClock, HostResetHook, HostTimer, Sdhci};
use sdmmc_protocol::{
    DataCommandProgress,
    block::BlockRequestId,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::host::{
        BusWidth, CardIrqControl, ClockSpeed, CompletionIrqRearm, CompletionIrqRearmHost,
        HostEvent, HostEventKind, HostEventSource, SdMmcIrqHandle, SdMmcIrqHost, SignalVoltage,
    },
};

use crate::{
    block_path::{submit_read_adma2, submit_write_adma2},
    dma::{BlockRequest, BlockRequestSlot, RequestId},
    regs::*,
};

/// Stable controller event extracted from SDHCI interrupt-status registers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
    #[default]
    None,
    /// A command response is ready to harvest.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// Receive-side FIFO data is ready.
    ReceiveReady,
    /// Transmit-side FIFO space is ready.
    TransmitReady,
    /// An SDIO function asserted the level-sensitive `CARD_INT` source.
    CardInterrupt,
    /// A card interrupt arrived together with another controller event.
    Combined {
        primary: HostEventKind,
        normal: u16,
        error: u16,
    },
    /// One or more error bits are pending.
    Error { normal: u16, error: u16 },
    /// Status bits are pending but do not map to a high-level event yet.
    Other { normal: u16, error: u16 },
}

pub struct DataRequest<'a> {
    id: RequestId,
    request: Option<BlockRequest>,
    slot: BlockRequestSlot,
    _buffer: PhantomData<&'a [u8]>,
}

pub struct TransactionRequest<'a> {
    owner: usize,
    id: u64,
    done: bool,
    acknowledged_irq: bool,
    kind: TransactionRequestKind,
    data: Option<DataRequest<'a>>,
}

enum TransactionRequestKind {
    Command { response: sdmmc_host::ResponseType },
    Data { response: sdmmc_host::ResponseType },
}

impl<'a> TransactionRequest<'a> {
    fn command(owner: usize, id: u64, response: sdmmc_host::ResponseType) -> Self {
        Self {
            owner,
            id,
            done: false,
            acknowledged_irq: false,
            kind: TransactionRequestKind::Command { response },
            data: None,
        }
    }

    fn data(
        owner: usize,
        id: u64,
        request: DataRequest<'a>,
        response: sdmmc_host::ResponseType,
    ) -> Self {
        Self {
            owner,
            id,
            done: false,
            acknowledged_irq: false,
            kind: TransactionRequestKind::Data { response },
            data: Some(request),
        }
    }
}

pub struct BusRequest {
    owner: usize,
    id: u64,
    done: bool,
    state: BusRequestState,
}

impl BusRequest {
    fn pending(owner: usize, id: u64, state: BusRequestState) -> Self {
        Self {
            owner,
            id,
            done: false,
            state,
        }
    }
}

enum BusRequestState {
    Reset {
        mask: u8,
        phase: Phase,
        was_irq_enabled: bool,
        started: bool,
        polls: u32,
    },
    PowerOn,
    PowerOff,
    SetClock(SdhciClockState),
    SetBusWidth(BusWidth),
    SetSignalVoltage(SdhciVoltageState),
    ExecuteTuning(SdhciTuningState),
}

enum SdhciClockState {
    Start {
        target_hz: u32,
        uhs_mode: Option<u16>,
        high_speed: Option<bool>,
    },
    ExternalSetClock {
        target_hz: u32,
    },
    ExternalPrepareHost {
        target_hz: u32,
    },
    ExternalStart {
        target_hz: u32,
    },
    ExternalEnable {
        polls: u32,
    },
    InternalWaitStable {
        polls: u32,
    },
}

enum SdhciVoltageState {
    DisableClock(SignalVoltage),
    SwitchControllerAndRail(SignalVoltage),
    WaitVsw {
        voltage: SignalVoltage,
        deadline_ms: Option<u64>,
    },
    EnableClock(SignalVoltage),
    VerifyDatLines(SignalVoltage),
}

enum SdhciTuningState {
    Start { cmd_index: u8, block_size: u16 },
    Wait { cmd_index: u8, polls: u32 },
}

const SDHCI_RESET_POLLS: u32 = 1_000;
const SDHCI_CLOCK_POLLS: u32 = 1_000;
const SDHCI_TUNING_POLLS: u32 = 1_000_000;
const SDHCI_VOLTAGE_SWITCH_DELAY_MS: u64 = 5;
const SDHCI_REGISTER_RETRY_DELAY: Duration = Duration::from_micros(100);

/// Owned SDHCI IRQ top-half endpoint.
pub struct SdhciIrqHandle {
    irq: Arc<host::IrqCore>,
}

/// Task-context mask/rearm endpoint for `CARD_INT`.
pub struct SdhciCardIrqHandle {
    irq: Arc<host::IrqCore>,
}

impl SdMmcIrqHost for Sdhci {
    type Event = Event;
    type IrqHandle = SdhciIrqHandle;
    type CardIrq = SdhciCardIrqHandle;

    fn into_parts(mut self) -> sdmmc_host::HostParts<Self, Self::IrqHandle, Self::CardIrq> {
        let irq = Sdhci::irq_endpoint(&mut self);
        let card_irq = Some(Sdhci::card_irq_endpoint(&mut self));
        sdmmc_host::HostParts {
            bus: self,
            irq,
            card_irq,
        }
    }

    fn completion_irq_enabled(&self) -> bool {
        Sdhci::completion_irq_enabled(self)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        Sdhci::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        Sdhci::disable_completion_irq(self);
        Ok(())
    }

    fn device_dma(&self) -> Result<&dma_api::DeviceDma, Error> {
        self.dma.as_ref().ok_or(Error::UnsupportedCommand)
    }

    fn progress_wait_kind(&self) -> sdmmc_protocol::sdio::HostProgressWait {
        Sdhci::progress_wait_kind(self)
    }
}

impl CompletionIrqRearmHost for Sdhci {
    fn rearm_completion_irq_and_check(&mut self) -> Result<CompletionIrqRearm, Error> {
        Ok(Sdhci::rearm_completion_irq_and_check(self))
    }
}

fn sdhci_clock_divisor(base_clock_hz: u32, target_hz: u32) -> u16 {
    if target_hz == 0 || base_clock_hz <= target_hz {
        return 0;
    }
    for n in 1..=0x3FF {
        if base_clock_hz / (2 * n as u32) <= target_hz {
            return n;
        }
    }
    0x3FF
}

pub(crate) fn sdhci_clock_divisor_with_quirk(
    base_clock_hz: u32,
    target_hz: u32,
    div_zero_broken: bool,
) -> u16 {
    let div = sdhci_clock_divisor(base_clock_hz, target_hz);
    if div_zero_broken && div == 0 && base_clock_hz <= 25_000_000 {
        1
    } else {
        div
    }
}

pub(crate) fn event_from_status(normal: u16, error: u16) -> Event {
    let has_card_interrupt = normal & NORMAL_INT_CARD_INTERRUPT != 0;
    let normal_without_card = normal & !NORMAL_INT_CARD_INTERRUPT;
    let primary = if normal_without_card & NORMAL_INT_ERROR != 0 {
        Event::Error { normal, error }
    } else if normal_without_card & NORMAL_INT_XFER_COMPLETE != 0 {
        Event::TransferComplete
    } else if normal_without_card & NORMAL_INT_BUFFER_READ_READY != 0 {
        Event::ReceiveReady
    } else if normal_without_card & NORMAL_INT_BUFFER_WRITE_READY != 0 {
        Event::TransmitReady
    } else if normal_without_card & NORMAL_INT_CMD_COMPLETE != 0 {
        Event::CommandComplete
    } else if normal_without_card != 0 || error != 0 {
        Event::Other { normal, error }
    } else {
        Event::None
    };
    if !has_card_interrupt {
        return primary;
    }
    match primary {
        Event::None => Event::CardInterrupt,
        event => Event::Combined {
            primary: event.kind(),
            normal,
            error,
        },
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
            Event::CardInterrupt => HostEventKind::CardInterrupt,
            Event::Combined { primary, .. } => *primary,
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
            Event::Combined { primary, .. } => source_from_kind(*primary),
            Event::None | Event::CardInterrupt | Event::Error { .. } | Event::Other { .. } => {
                HostEventSource::Controller
            }
        }
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        match self {
            Event::TransferComplete | Event::ReceiveReady | Event::TransmitReady => {
                Some(BlockRequestId::new(0))
            }
            Event::Combined {
                primary:
                    HostEventKind::TransferComplete
                    | HostEventKind::ReceiveReady
                    | HostEventKind::TransmitReady,
                ..
            } => Some(BlockRequestId::new(0)),
            Event::None
            | Event::CommandComplete
            | Event::CardInterrupt
            | Event::Combined { .. }
            | Event::Error { .. }
            | Event::Other { .. } => None,
        }
    }

    fn card_interrupt(&self) -> bool {
        matches!(self, Event::CardInterrupt | Event::Combined { .. })
    }
}

fn source_from_kind(kind: HostEventKind) -> HostEventSource {
    match kind {
        HostEventKind::CommandComplete => HostEventSource::Command,
        HostEventKind::TransferComplete
        | HostEventKind::ReceiveReady
        | HostEventKind::TransmitReady => HostEventSource::Data,
        _ => HostEventSource::Controller,
    }
}

impl Sdhci {
    pub(crate) fn irq_endpoint(&mut self) -> SdhciIrqHandle {
        SdhciIrqHandle {
            irq: self.irq.clone(),
        }
    }

    pub(crate) fn card_irq_endpoint(&mut self) -> SdhciCardIrqHandle {
        SdhciCardIrqHandle {
            irq: self.irq.clone(),
        }
    }

    fn rearm_completion_irq_and_check(&mut self) -> CompletionIrqRearm {
        self.enable_completion_irq();
        fence(Ordering::SeqCst);
        match handle_irq_core(&self.irq).kind() {
            HostEventKind::None | HostEventKind::CardInterrupt => CompletionIrqRearm::Idle,
            _ => CompletionIrqRearm::Pending,
        }
    }
}

impl SdMmcIrqHandle for SdhciIrqHandle {
    type Event = Event;

    fn handle_irq(&mut self) -> Self::Event {
        handle_irq_core(&self.irq)
    }
}

fn handle_irq_core(irq: &host::IrqCore) -> Event {
    let generation = irq.state.generation();
    let (raw_normal, raw_error) = host::read_irq_register(irq.base_addr, REG_NORMAL_INT_STATUS);
    let (normal_status_enable, error_status_enable) =
        host::read_irq_register(irq.base_addr, REG_NORMAL_INT_STATUS_ENABLE);
    let (signal_enable, error_signal_enable) =
        host::read_irq_register(irq.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE);

    let normal_enabled = raw_normal & normal_status_enable;
    let error_enabled = if normal_enabled & NORMAL_INT_ERROR != 0 {
        raw_error & error_status_enable
    } else {
        0
    };

    // STATUS_ENABLE controls which latched bits the driver owns and
    // SIGNAL_ENABLE gates assertion of the external IRQ line. A CARD_INT can
    // arrive together with command/data completion; preserve the completion
    // facts in this snapshot before masking the level source below.
    let visible_card = normal_enabled & signal_enable & NORMAL_INT_CARD_INTERRUPT;
    let normal = (normal_enabled & !NORMAL_INT_CARD_INTERRUPT) | visible_card;
    let error = error_enabled;

    let normal_to_ack = raw_normal & !NORMAL_INT_CARD_INTERRUPT;
    if normal_to_ack != 0 || raw_error != 0 {
        host::write_irq_register(
            irq.base_addr,
            REG_NORMAL_INT_STATUS,
            normal_to_ack,
            raw_error,
        );
    }

    let normal = if error == 0 {
        normal & !NORMAL_INT_ERROR
    } else {
        normal
    };
    if normal & NORMAL_INT_CARD_INTERRUPT != 0 {
        // Linux's SDHCI `ier` is written to both INT_ENABLE and
        // SIGNAL_ENABLE when CARD_INT is consumed.  Do the same here: the
        // source is level-sensitive and must remain masked until the task
        // context drains the AIC function FIFOs and explicitly rearms it.
        host::write_irq_register(
            irq.base_addr,
            REG_NORMAL_INT_STATUS_ENABLE,
            normal_status_enable & !NORMAL_INT_CARD_INTERRUPT,
            error_status_enable,
        );
        host::write_irq_register(
            irq.base_addr,
            REG_NORMAL_INT_SIGNAL_ENABLE,
            signal_enable & !NORMAL_INT_CARD_INTERRUPT,
            error_signal_enable,
        );
    }
    irq.state
        .cache_if_current(generation, normal & !NORMAL_INT_CARD_INTERRUPT, error);

    event_from_status(normal, error)
}

impl CardIrqControl for SdhciCardIrqHandle {
    fn mask(&mut self) {
        let (status_enable, error_status_enable) =
            host::read_irq_register(self.irq.base_addr, REG_NORMAL_INT_STATUS_ENABLE);
        host::write_irq_register(
            self.irq.base_addr,
            REG_NORMAL_INT_STATUS_ENABLE,
            status_enable & !NORMAL_INT_CARD_INTERRUPT,
            error_status_enable,
        );
        let (signals, error_signals) =
            host::read_irq_register(self.irq.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE);
        host::write_irq_register(
            self.irq.base_addr,
            REG_NORMAL_INT_SIGNAL_ENABLE,
            signals & !NORMAL_INT_CARD_INTERRUPT,
            error_signals,
        );
    }

    fn disable(&mut self) {
        self.mask();
    }

    fn rearm_and_check(&mut self) -> bool {
        let (status_enable, error_status_enable) =
            host::read_irq_register(self.irq.base_addr, REG_NORMAL_INT_STATUS_ENABLE);
        host::write_irq_register(
            self.irq.base_addr,
            REG_NORMAL_INT_STATUS_ENABLE,
            status_enable | NORMAL_INT_CARD_INTERRUPT,
            error_status_enable,
        );
        let (signals, error_signals) =
            host::read_irq_register(self.irq.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE);
        host::write_irq_register(
            self.irq.base_addr,
            REG_NORMAL_INT_SIGNAL_ENABLE,
            signals | NORMAL_INT_CARD_INTERRUPT,
            error_signals,
        );
        fence(Ordering::SeqCst);
        if host::read_irq_register(self.irq.base_addr, REG_NORMAL_INT_STATUS).0
            & NORMAL_INT_CARD_INTERRUPT
            != 0
        {
            self.mask();
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
