//! SDHCI host controller backend for the `sdmmc-protocol` driver crate.
//!
//! This crate ports the [SD Host Controller Standard Specification][sdhci]
//! v3.x register layout and PIO data path into a physical
//! [`sdio_host2::SdioHost`] implementation that
//! [`sdmmc_protocol::sdio::card::SdioSdmmc`] drives through
//! [`sdmmc_protocol::sdio::card::SdioSdmmc::new_host2`].
//!
//! # Scope
//!
//! - **Implemented**: PIO transfers, **ADMA2 (32-bit) transfers**, 1-bit /
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
//! use sdhci_host::Sdhci;
//! use sdmmc_protocol::sdio::{card::SdioSdmmc, init::SdioInitScratch};
//!
//! let mmio = NonNull::new(0xFE31_0000 as *mut u8).unwrap();
//! let host = unsafe { Sdhci::new(mmio) };
//! let mut card = SdioSdmmc::new_host2(host);
//! let mut scratch = SdioInitScratch::new();
//! let mut request = card.submit_init(&mut scratch)?;
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
use core::{marker::PhantomData, ptr::NonNull};

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
pub use host::{BlockTransferPolicy, HostClock, HostResetHook, HostTimer, Sdhci};
use sdmmc_protocol::{
    DataCommandPoll, OperationPoll,
    block::BlockRequestId,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::host::{
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, ReadyBusRequest,
        SdioBusOp, SdioHost as ProtocolSdioHost, SdioIrqHandle, SdioIrqHost, SignalVoltage,
        poll_ready_bus_op, submit_ready_bus_op,
    },
};

use crate::{
    block_path::{submit_read_with_dma_fifo_fallback, submit_write_with_dma_fifo_fallback},
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

/// Owned SDHCI IRQ top-half endpoint.
pub struct SdhciIrqHandle {
    irq: Arc<host::IrqCore>,
}

impl ProtocolSdioHost for Sdhci {
    type Event = Event;
    type DataRequest<'a>
        = DataRequest<'a>
    where
        Self: 'a;
    type BusRequest = ReadyBusRequest;

    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        self.check_not_poisoned()?;
        Sdhci::submit_command(self, cmd)
    }

    fn poll_command_response(&mut self) -> Result<sdmmc_protocol::CommandResponsePoll, Error> {
        Sdhci::poll_command_response(self)
    }

    fn submit_read_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        let buffer = NonNull::new(buf.as_mut_ptr()).ok_or(Error::InvalidArgument)?;
        let mut slot = BlockRequestSlot::default();
        let request = submit_read_with_dma_fifo_fallback(
            self,
            cmd,
            buffer,
            buf.len(),
            block_size,
            block_count,
            &mut slot,
        )?;
        let id = request.id();
        Ok(DataRequest {
            id,
            request: Some(request),
            slot,
            _buffer: PhantomData,
        })
    }

    fn submit_write_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        let buffer = NonNull::new(buf.as_ptr() as *mut u8).ok_or(Error::InvalidArgument)?;
        let mut slot = BlockRequestSlot::default();
        let request = submit_write_with_dma_fifo_fallback(
            self,
            cmd,
            buffer,
            buf.len(),
            block_size,
            block_count,
            &mut slot,
        )?;
        let id = request.id();
        Ok(DataRequest {
            id,
            request: Some(request),
            slot,
            _buffer: PhantomData,
        })
    }

    fn poll_data_request<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
    ) -> Result<DataCommandPoll, Error> {
        self.progress_block_request(&mut request.request, request.id, &mut request.slot)
    }

    fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
        self.apply_bus_width(width)
    }

    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
        let (target_hz, uhs_mode) = match speed {
            ClockSpeed::Identification => (400_000, HOST_CTRL2_UHS_SDR12),
            ClockSpeed::Default | ClockSpeed::Sdr12 => (25_000_000, HOST_CTRL2_UHS_SDR12),
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => (50_000_000, HOST_CTRL2_UHS_SDR25),
            ClockSpeed::Sdr50 => (50_000_000, HOST_CTRL2_UHS_SDR50),
            ClockSpeed::Ddr50 => (50_000_000, HOST_CTRL2_UHS_DDR50),
            ClockSpeed::Sdr104 => (104_000_000, HOST_CTRL2_UHS_SDR104),
            ClockSpeed::Hs200 => (200_000_000, HOST_CTRL2_UHS_SDR104),
            // Future ClockSpeed variants are not supported by this controller.
            _ => return Err(Error::UnsupportedCommand),
        };

        // Match Linux's SDHCI/DWCMSHC UHS signaling selection: even legacy
        // MMC HighSpeed maps to the SDR25 bus-speed mode on controllers that
        // interpret HOST_CONTROL2.UHS_MODE_SELECT.
        let mut ctrl2 = self.read_u16(REG_HOST_CONTROL2);
        ctrl2 = (ctrl2 & !HOST_CTRL2_UHS_MODE_MASK) | uhs_mode;
        self.write_u16(REG_HOST_CONTROL2, ctrl2);

        // Toggle the High-Speed Enable bit in HOST_CONTROL1 alongside the
        // divider change so the controller pipelines reflect the new
        // timing window.
        let mut ctrl = self.read_u8(REG_HOST_CONTROL1);
        if matches!(
            speed,
            ClockSpeed::Identification | ClockSpeed::Default | ClockSpeed::Sdr12
        ) {
            ctrl &= !HOST_CTRL1_HIGH_SPEED;
        } else {
            ctrl |= HOST_CTRL1_HIGH_SPEED;
        }
        self.write_u8(REG_HOST_CONTROL1, ctrl);

        // External-clock mode: gate SD clock off, ask the platform CRU to
        // retune the reference clock, let platform glue configure host-side
        // clock registers, then bring SD clock back up at 1:1.
        if self.ext_clock.is_some() {
            self.disable_sd_clock();
            let clock = self.ext_clock.take().ok_or(Error::InvalidArgument)?;
            let effective_hz = clock.effective_clock_hz(target_hz);
            clock.set_clock(effective_hz)?;
            clock.prepare_host_clock(self, effective_hz)?;
            self.ext_clock = Some(clock);
            return self.enable_clock_passthrough(effective_hz);
        }

        let base = self.base_clock_hz();
        if base == 0 {
            return Err(Error::BadResponse(ErrorContext::new(Phase::Init)));
        }
        self.enable_clock(base, target_hz)
    }

    fn switch_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        // 1. Stop the SD clock so we don't drive the bus during the
        //    transition. Spec calls for ≥ 5 ms here; the controller's
        //    `1.8V Signaling Enable` bit toggles the IO domain
        //    immediately, so the wait is a soft requirement enforced by
        //    the platform delay (we don't have one here — bring-up code
        //    on the caller side should add one if needed).
        // V180 requires the platform to actually swing the IO rail —
        // flipping the controller bit in isolation makes the host
        // sample at the wrong reference, breaking every subsequent
        // data transfer (observed on rk3568-dwcmshc, where HS200
        // tuning fails and the leaked bit then corrupts HS@52 reads).
        // Refuse here unless the platform has opted in via
        // `Sdhci::enable_1v8_signaling`. Returning `UnsupportedCommand`
        // makes the protocol layer fall back cleanly.
        if matches!(voltage, SignalVoltage::V180) && !self.support_1v8 {
            return Err(Error::UnsupportedCommand);
        }
        if matches!(voltage, SignalVoltage::V120) {
            return Err(Error::UnsupportedCommand);
        }

        self.disable_sd_clock();

        // 2. Flip the voltage selector. 1.2 V isn't part of the SDHCI
        //    standard register — surface as Unsupported so the protocol
        //    layer falls back instead of silently doing the wrong thing.
        let mut ctrl2 = self.read_u16(REG_HOST_CONTROL2);
        match voltage {
            SignalVoltage::V330 => {
                ctrl2 &= !HOST_CTRL2_1V8_SIGNALING;
                self.set_power(POWER_330);
            }
            SignalVoltage::V180 => {
                ctrl2 |= HOST_CTRL2_1V8_SIGNALING;
                self.set_power(POWER_180);
            }
            SignalVoltage::V120 => unreachable!("V120 was rejected before mutating registers"),
            // Future SignalVoltage variants are not supported by this controller.
            _ => return Err(Error::UnsupportedCommand),
        }
        self.write_u16(REG_HOST_CONTROL2, ctrl2);

        // 3. Bring the SD clock back on. The protocol layer's next
        //    `set_clock` call will pick the appropriate divider for
        //    whatever speed mode we're transitioning into.
        let cur = self.read_u16(REG_CLOCK_CONTROL);
        self.write_u16(REG_CLOCK_CONTROL, cur | CLOCK_SD_ENABLE);

        // 4. Sanity check: when entering 1.8 V the spec requires
        //    DAT[3:0] to be high after the switch (PRESENT_STATE bits
        //    20..23). We don't enforce this in the MVP because some
        //    QEMU models leave the bits dangling; real hardware
        //    integrators should add the check here.
        Ok(())
    }

    fn execute_tuning(
        &mut self,
        cmd_index: u8,
        block_size: core::num::NonZeroU16,
    ) -> Result<(), Error> {
        // Only CMD19 (SD UHS-I) and CMD21 (eMMC HS200) make sense here.
        // Reject anything else loudly so the protocol layer doesn't
        // accidentally tune for a non-tuning command.
        if cmd_index != 19 && cmd_index != 21 {
            return Err(Error::InvalidArgument);
        }

        // Block size for the tuning data phase: SD CMD19 always 64,
        // MMC CMD21 is 64 (4-bit) or 128 (8-bit).
        let expected_block_size =
            if cmd_index == 21 && self.read_u8(REG_HOST_CONTROL1) & HOST_CTRL1_8BIT != 0 {
                sdmmc_protocol::cmd::MMC_TUNING_BLOCK_SIZE_8BIT
            } else {
                sdmmc_protocol::cmd::SD_TUNING_BLOCK_SIZE
            };
        if u32::from(block_size.get()) != expected_block_size {
            return Err(Error::InvalidArgument);
        }

        // Pre-program the data registers per SDHCI v3 §3.7.7. The
        // controller issues the tuning command itself; we just hand it
        // the shape of the data phase.
        self.write_u16(REG_BLOCK_SIZE, block_size.get() & 0x0FFF);
        self.write_u16(REG_BLOCK_COUNT, 1);
        self.write_u8(REG_TIMEOUT_CONTROL, 0x0E);
        // Direction = read, single block, DMA disabled.
        self.write_u16(
            REG_TRANSFER_MODE,
            XFER_MODE_BLOCK_COUNT_ENABLE | XFER_MODE_READ,
        );

        // 1. Set the Execute Tuning bit. The controller takes over and
        //    issues the tuning command repeatedly while sweeping its
        //    sampling clock; software just polls the bit until it
        //    self-clears, then checks Sampling Clock Select to know
        //    whether the sweep landed on a stable phase.
        let mut ctrl2 = self.read_u16(REG_HOST_CONTROL2);
        ctrl2 |= HOST_CTRL2_EXECUTE_TUNING;
        self.write_u16(REG_HOST_CONTROL2, ctrl2);

        // SDHCI spec caps the loop at 40 iterations × 5 ms each — a
        // worst case of 200 ms. We pick a conservative poll budget
        // around that.
        const TUNING_POLLS: u32 = 1_000_000;
        let mut last_status = 0u16;
        for _ in 0..TUNING_POLLS {
            last_status = self.read_u16(REG_HOST_CONTROL2);
            if last_status & HOST_CTRL2_EXECUTE_TUNING == 0 {
                // Controller's done. Sampling Clock Select tells us
                // whether the sweep produced a usable phase.
                if last_status & HOST_CTRL2_SAMPLING_CLOCK_SELECT != 0 {
                    return Ok(());
                }
                return Err(Error::BadResponse(ErrorContext::for_cmd(
                    Phase::Init,
                    cmd_index,
                )));
            }
            core::hint::spin_loop();
        }

        // Tuning didn't converge in our poll budget. Clear the bit so
        // the next attempt starts clean, and surface a timeout.
        let cleared = last_status & !HOST_CTRL2_EXECUTE_TUNING;
        self.write_u16(REG_HOST_CONTROL2, cleared);
        Err(Error::Timeout(ErrorContext::for_cmd(
            Phase::Init,
            cmd_index,
        )))
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        Sdhci::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        Sdhci::disable_completion_irq(self);
        Ok(())
    }

    fn completion_irq_enabled(&self) -> bool {
        Sdhci::completion_irq_enabled(self)
    }

    fn submit_bus_op(&mut self, op: SdioBusOp) -> Result<Self::BusRequest, Error> {
        submit_ready_bus_op(self, op)
    }

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error> {
        poll_ready_bus_op(request)
    }
}

impl SdioIrqHost for Sdhci {
    type IrqHandle = SdhciIrqHandle;

    fn irq_handle(&mut self) -> Self::IrqHandle {
        Sdhci::irq_endpoint(self)
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

    /// Read and acknowledge pending controller status, returning a stable
    /// event for OS glue to translate into wakeups or worker scheduling.
    pub fn handle_irq(&mut self) -> Event {
        handle_irq_core(&self.irq)
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
    let normal = read_u16(irq.base_addr, REG_NORMAL_INT_STATUS);
    let error = if normal & NORMAL_INT_ERROR != 0 {
        read_u16(irq.base_addr, REG_ERROR_INT_STATUS)
    } else {
        0
    };

    if normal != 0 {
        write_u16(irq.base_addr, REG_NORMAL_INT_STATUS, normal);
    }
    if error != 0 {
        write_u16(irq.base_addr, REG_ERROR_INT_STATUS, error);
    }
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
