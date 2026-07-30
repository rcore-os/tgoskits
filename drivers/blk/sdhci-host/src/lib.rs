//! SDHCI host controller backend for the `sdmmc-protocol` driver crate.
//!
//! This crate ports the [SD Host Controller Standard Specification][sdhci]
//! v3.x register layout and ADMA2 data path into a physical
//! [`sdio_host2::SdioHost`] implementation that
//! [`sdmmc_protocol::sdio::card::SdioSdmmc`] drives through
//! [`sdmmc_protocol::sdio::card::SdioSdmmc::new`].
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
//! use sdmmc_protocol::sdio::card::SdioSdmmc;
//!
//! let mmio = NonNull::new(0xFE31_0000 as *mut u8).unwrap();
//! let dma: DeviceDma = todo!("install the platform DMA capability");
//! let mut host = unsafe { Sdhci::new(mmio) };
//! host.configure_dma(dma)?;
//! let mut card = SdioSdmmc::new(host);
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
use core::{marker::PhantomData, ptr::NonNull, time::Duration};

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
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, SdioIrqHandle,
        SdioIrqHost, SignalVoltage,
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
    Command { response: sdio_host2::ResponseType },
    Data { response: sdio_host2::ResponseType },
}

impl<'a> TransactionRequest<'a> {
    fn command(owner: usize, id: u64, response: sdio_host2::ResponseType) -> Self {
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
        response: sdio_host2::ResponseType,
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

impl SdioIrqHost for Sdhci {
    type Event = Event;
    type IrqHandle = SdhciIrqHandle;

    fn irq_handle(&mut self) -> Self::IrqHandle {
        Sdhci::irq_endpoint(self)
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
    if normal & NORMAL_INT_ERROR != 0 {
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
}

impl Sdhci {
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
    let raw_normal = read_u16(irq.base_addr, REG_NORMAL_INT_STATUS);
    let raw_error = if raw_normal & NORMAL_INT_ERROR != 0 {
        read_u16(irq.base_addr, REG_ERROR_INT_STATUS)
    } else {
        0
    };

    if raw_normal != 0 {
        write_u16(irq.base_addr, REG_NORMAL_INT_STATUS, raw_normal);
    }
    if raw_error != 0 {
        write_u16(irq.base_addr, REG_ERROR_INT_STATUS, raw_error);
    }

    let normal = raw_normal
        & read_u16(irq.base_addr, REG_NORMAL_INT_STATUS_ENABLE)
        & read_u16(irq.base_addr, REG_NORMAL_INT_SIGNAL_ENABLE);
    let error = raw_error
        & read_u16(irq.base_addr, REG_ERROR_INT_STATUS_ENABLE)
        & read_u16(irq.base_addr, REG_ERROR_INT_SIGNAL_ENABLE);
    let normal = if error == 0 {
        normal & !NORMAL_INT_ERROR
    } else {
        normal
    };
    irq.state.cache_if_current(generation, normal, error);

    event_from_status(normal, error)
}

fn read_u16(base_addr: usize, off: usize) -> u16 {
    unsafe { core::ptr::read_volatile((base_addr + off) as *const u16) }
}

fn write_u16(base_addr: usize, off: usize, val: u16) {
    unsafe { core::ptr::write_volatile((base_addr + off) as *mut u16, val) }
}

#[cfg(test)]
mod tests;
