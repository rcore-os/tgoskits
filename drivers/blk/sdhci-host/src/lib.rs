//! SDHCI host controller backend for the `sdmmc-protocol` driver crate.
//!
//! This crate ports the [SD Host Controller Standard Specification][sdhci]
//! v3.x register layout and PIO data path into a physical
//! [`sdio_host2::SdioHost`] implementation that
//! [`sdmmc_protocol::sdio::card::SdioSdmmc`] drives through
//! [`sdmmc_protocol::sdio::card::SdioSdmmc::new_host2_timed`].
//!
//! # Scope
//!
//! - **Implemented**: PIO transfers, **ADMA2 (32-bit) transfers**, 1-bit /
//!   4-bit / 8-bit bus, default-speed and high-speed clocking, 32-bit response
//!   slots, 136-bit R2 reconstruction, software reset / clock setup.
//! - **Out of scope (for now)**: 64-bit ADMA2, HS200 / SDR50 / SDR104
//!   clocking, tuning (CMD19 / CMD21), eMMC-specific commands beyond normal
//!   block I/O. 1.8 V signaling is wired up at the register level but is
//!   gated behind [`Sdhci::enable_1v8_signaling`] — platforms that haven't
//!   plumbed the IO-rail regulator MUST leave it off so the protocol
//!   layer falls back instead of corrupting transfers.
//!
//! # Usage
//!
//! ```no_run
//! use core::ptr::NonNull;
//!
//! use sdhci_host::Sdhci;
//! use sdmmc_protocol::sdio::{card::SdioSdmmc, init::SdioInitScratch};
//!
//! let Some(mmio) = NonNull::new(0xFE31_0000 as *mut u8) else {
//!     unreachable!()
//! };
//! let mut host = unsafe { Sdhci::new(mmio) };
//! // OS glue moves this endpoint into its registered IRQ action before the
//! // initialization FSM is allowed to issue its first card command.
//! let _registered_irq = host.irq_endpoint();
//! host.enable_completion_irq();
//! let mut card = SdioSdmmc::new_host2_timed(host);
//! let mut scratch = SdioInitScratch::new();
//! let mut request = card.submit_init(&mut scratch)?;
//! // Re-enter initialization only when its IRQ/deadline policy permits.
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! Low-level block request primitives remain available for controller bring-up
//! and diagnostics. Normal block-device users should prefer [`rdif::device`],
//! which routes RDIF requests through the shared SD/MMC protocol state machine
//! and this host's native `sdio-host2` transaction path. In runtime IRQ mode,
//! [`Sdhci::service_block_request`] is a state-advance operation over one
//! acknowledged IRQ snapshot; it must never be called as a periodic
//! completion probe. Errors retain request and DMA ownership until the
//! controller lifecycle produces a quiescence proof.
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
    num::NonZeroUsize,
    ptr::NonNull,
    sync::atomic::{AtomicBool, Ordering},
};

mod command;
mod dma;
mod host;
pub mod rdif;
mod regs;

pub use dma::{
    ADMA2_DESC_ALIGN, ADMA2_DESC_COUNT, ADMA2_MAX_BLOCKS, ADMA2_MAX_TRANSFER_SIZE, BlockRequest,
    BlockRequestSlot, RequestId,
};
pub use host::{
    BroadcomController, HostClock, HostResetHook, HostTimer, ResetHookPoll, ResetHookRecoveryMode,
    Sdhci,
};
pub use sdmmc_protocol::block::{
    BlockBufferConfig, BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode,
    BlockTransferState,
};
use sdmmc_protocol::{
    DataCommandPoll, OperationPoll,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::{
        host::{
            BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, ReadyBusRequest,
            SdioBusOp, SdioHost as ProtocolSdioHost, SdioIrqHandle, SdioIrqHost, SignalVoltage,
            poll_ready_bus_op,
        },
        host2::{SdioHost2Lifecycle, SdioHost2Timed},
    },
};

use crate::regs::*;

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
    kind: TransactionRequestKind,
    data: Option<DataRequest<'a>>,
}

static ADMA_READ_PATH_LOGGED: AtomicBool = AtomicBool::new(false);
static ADMA_WRITE_PATH_LOGGED: AtomicBool = AtomicBool::new(false);
static ADMA_READ_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);
static ADMA_WRITE_FALLBACK_LOGGED: AtomicBool = AtomicBool::new(false);

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
        state: SdhciResetState,
    },
    PowerOn,
    PowerOff,
    SetClock(SdhciClockState),
    SetBusWidth(BusWidth),
    SetSignalVoltage(SdhciVoltageState),
    ExecuteTuning(SdhciTuningState),
}

enum SdhciResetState {
    Start,
    WaitHook { wake_at_ns: u64 },
    WaitController { wait: Host2TimedWait },
}

const RECOVERY_CHECK_INTERVAL_NS: u64 = 50_000;
const RECOVERY_TRANSITION_TIMEOUT_NS: u64 = 100_000_000;

/// Bounded SDHCI reset/reconstruction state retained by the RDIF lifecycle.
pub struct SdhciRecoveryState {
    phase: SdhciRecoveryPhase,
    saved: SdhciRecoveryRegisters,
}

#[derive(Clone, Copy)]
struct SdhciRecoveryRegisters {
    power_control: u8,
    clock_control: u16,
    host_control1: u8,
    host_control2: u16,
    timeout_control: u8,
    normal_status_enable: u16,
    error_status_enable: u16,
}

enum SdhciRecoveryPhase {
    Start,
    WaitHook { wake_at_ns: u64 },
    WaitReset { deadline_ns: u64 },
    Quiesced,
    Restore,
    WaitClock { deadline_ns: u64 },
    Ready,
    Failed,
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
        target_hz: u32,
        wait: Host2TimedWait,
    },
    InternalWaitStable {
        target_hz: u32,
        wait: Host2TimedWait,
    },
}

enum SdhciVoltageState {
    DisableClock(SignalVoltage),
    SwitchControllerAndRail(SignalVoltage),
    WaitVsw {
        voltage: SignalVoltage,
        wake_at_ns: u64,
    },
    EnableClock(SignalVoltage),
    VerifyDatLines(SignalVoltage),
}

enum SdhciTuningState {
    Start { cmd_index: u8, block_size: u16 },
    Wait { cmd_index: u8, wait: Host2TimedWait },
}

#[derive(Clone, Copy)]
struct Host2TimedWait {
    deadline_ns: u64,
    wake_at_ns: u64,
}

impl Host2TimedWait {
    fn start(now_ns: u64) -> Self {
        let deadline_ns = now_ns.saturating_add(HOST2_TRANSITION_TIMEOUT_NS);
        Self {
            deadline_ns,
            wake_at_ns: next_host2_check(now_ns, deadline_ns),
        }
    }

    const fn expired(self, now_ns: u64) -> bool {
        now_ns >= self.deadline_ns
    }

    fn defer(&mut self, now_ns: u64) {
        self.wake_at_ns = next_host2_check(now_ns, self.deadline_ns);
    }
}

const HOST2_CHECK_INTERVAL_NS: u64 = 50_000;
const HOST2_TRANSITION_TIMEOUT_NS: u64 = 100_000_000;
const SDHCI_VOLTAGE_SWITCH_DELAY_NS: u64 = 5_000_000;

fn next_host2_check(now_ns: u64, deadline_ns: u64) -> u64 {
    now_ns
        .saturating_add(HOST2_CHECK_INTERVAL_NS)
        .min(deadline_ns)
}

/// Owned SDHCI IRQ top-half endpoint.
pub struct SdhciIrqHandle {
    irq: Arc<host::IrqCore>,
}

mod bus;
mod irq;
mod protocol;
mod recovery;
#[cfg(test)]
mod tests;
mod transfer;
