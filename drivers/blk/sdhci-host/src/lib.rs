//! SDHCI host controller backend for the `sdmmc-protocol` driver crate.
//!
//! This crate ports the [SD Host Controller Standard Specification][sdhci]
//! v3.x register layout and PIO data path into a physical
//! [`sdio_host2::SdioHost`] implementation that
//! [`sdmmc_protocol::sdio::SdioSdmmc`] drives through
//! [`sdmmc_protocol::sdio::SdioSdmmc::new_host2`].
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
//! use sdmmc_protocol::sdio::{SdioInitScratch, SdioSdmmc};
//!
//! let mmio = NonNull::new(0xFE31_0000 as *mut u8).unwrap();
//! let host = unsafe { Sdhci::new(mmio) };
//! let mut card = SdioSdmmc::new_host2(host);
//! let mut scratch = SdioInitScratch::new();
//! let mut request = card.submit_init(&mut scratch)?;
//! // Poll request here. Runtime code chooses spin, yield, IRQ wait, or timer.
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! Low-level block request primitives remain available for controller bring-up
//! and diagnostics. Normal block-device users should prefer [`rdif::device`],
//! which routes RDIF requests through the shared SD/MMC protocol state machine
//! and this host's native `sdio-host2` transaction path. The raw primitives use
//! [`Sdhci::submit_read_blocks`] or [`Sdhci::submit_write_blocks`] and complete
//! the returned request with [`Sdhci::poll_block_request`]:
//!
//! ```ignore
//! use core::{num::NonZeroUsize, ptr::NonNull};
//! use dma_api::DeviceDma;
//! use sdhci_host::{BlockRequestSlot, BlockTransferMode, RequestId, Sdhci};
//!
//! # use platform::DmaImpl;
//! let dma = DeviceDma::new_legacy(u32::MAX as u64, &DmaImpl);
//! let mut host = unsafe { Sdhci::new_from_addr(0xFE31_0000) };
//! let mut block = [0u8; 512];
//! let ptr = NonNull::new(block.as_mut_ptr()).unwrap();
//! let mut slot = BlockRequestSlot::default();
//! let mut request = Some(host.submit_read_blocks(
//!     0,
//!     ptr,
//!     NonZeroUsize::new(block.len()).unwrap(),
//!     Some(&dma),
//!     BlockTransferMode::Dma,
//!     &mut slot,
//! )?);
//! let id = RequestId::new(0);
//! while matches!(host.poll_block_request(&mut request, id, &mut slot), Ok(BlockPoll::Pending)) {}
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
pub use host::{HostClock, HostResetHook, HostTimer, Sdhci};
pub use sdmmc_protocol::block::{
    BlockBufferConfig, BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode,
    BlockTransferState,
};
use sdmmc_protocol::{
    DataCommandPoll, OperationPoll,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::{
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, ReadyBusRequest,
        SdioBusOp, SdioHost as ProtocolSdioHost, SdioIrqHandle, SdioIrqHost, SignalVoltage,
        poll_ready_bus_op, submit_ready_bus_op,
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

/// Cloneable, sync-safe SDHCI IRQ top-half handle.
#[derive(Clone)]
pub struct SdhciIrqHandle {
    irq: Arc<host::IrqCore>,
}

impl ProtocolSdioHost for Sdhci {
    type Event = Event;
    type DataRequest<'a> = DataRequest<'a>;
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
        self.poll_block_request_response(&mut request.request, request.id, &mut request.slot)
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
        // retune the reference clock, then bring SD clock back up at 1:1.
        if let Some(cb) = self.ext_clock {
            self.disable_sd_clock();
            cb.set_clock(target_hz)?;
            return self.enable_clock_external();
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

    fn handle_irq(&mut self) -> Self::Event {
        self.irq_handle().handle_irq()
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

    fn irq_handle(&self) -> Self::IrqHandle {
        Sdhci::irq_handle(self)
    }

    fn completion_irq_enabled(&self) -> bool {
        Sdhci::completion_irq_enabled(self)
    }
}

impl sdio_host2::SdioHost for Sdhci {
    type TransactionRequest<'a>
        = TransactionRequest<'a>
    where
        Self: 'a;
    type BusRequest = BusRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: sdio_host2::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::Error>
    where
        Self: 'a,
    {
        self.check_not_poisoned().map_err(map_protocol_error)?;
        if !self.physical_bus_idle() {
            return Err(sdio_host2::Error::Busy);
        }
        let owner = self.host2_owner();
        let id = self.start_host2_request();
        let response = transaction.command.response;
        match transaction.data {
            None => {
                if let Err(err) = self.submit_command(&transaction.command) {
                    self.finish_host2_request(id);
                    return Err(map_protocol_error(err));
                }
                Ok(TransactionRequest::command(owner, id, response))
            }
            Some(phase) => {
                phase
                    .validate()
                    .inspect_err(|_| self.finish_host2_request(id))?;
                let block_size = u32::from(phase.block_size.get());
                let block_count = phase.block_count.get();
                let request = match phase.buffer {
                    sdio_host2::DataBuffer::Read(buf) => {
                        if !matches!(phase.direction, sdio_host2::DataDirection::Read) {
                            self.finish_host2_request(id);
                            return Err(sdio_host2::Error::InvalidArgument);
                        }
                        <Self as ProtocolSdioHost>::submit_read_data(
                            self,
                            &transaction.command,
                            buf,
                            block_size,
                            block_count,
                        )
                    }
                    sdio_host2::DataBuffer::Write(buf) => {
                        if !matches!(phase.direction, sdio_host2::DataDirection::Write) {
                            self.finish_host2_request(id);
                            return Err(sdio_host2::Error::InvalidArgument);
                        }
                        <Self as ProtocolSdioHost>::submit_write_data(
                            self,
                            &transaction.command,
                            buf,
                            block_size,
                            block_count,
                        )
                    }
                    sdio_host2::DataBuffer::Dma(_) => {
                        self.finish_host2_request(id);
                        return Err(sdio_host2::Error::InvalidArgument);
                    }
                }
                .inspect_err(|_| self.finish_host2_request(id))
                .map_err(map_protocol_error)?;
                Ok(TransactionRequest::data(owner, id, request, response))
            }
        }
    }

    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: sdio_host2::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        if let Err(err) = self.check_not_poisoned() {
            return Err(sdio_host2::SubmitTransactionError::new(
                map_protocol_error(err),
                transaction,
            ));
        }
        if !matches!(
            transaction.data.as_ref().map(|data| &data.buffer),
            Some(sdio_host2::DataBuffer::Dma(_))
        ) {
            return unsafe { self.submit_transaction(transaction) }
                .map_err(sdio_host2::SubmitTransactionError::consumed);
        }
        if !self.physical_bus_idle() {
            return Err(sdio_host2::SubmitTransactionError::new(
                sdio_host2::Error::Busy,
                transaction,
            ));
        }

        let owner = self.host2_owner();
        let host2_id = self.start_host2_request();
        let response = transaction.command.response;
        let Some(phase) = transaction.data else {
            unreachable!("DMA transaction must contain a data phase")
        };
        let block_size = u32::from(phase.block_size.get());
        let block_count = phase.block_count.get();
        let sdio_host2::DataBuffer::Dma(buffer) = phase.buffer else {
            unreachable!("checked for DMA data buffer above")
        };
        if !should_try_dma(
            &transaction.command,
            block_size,
            block_count,
            buffer.len().get(),
            match phase.direction {
                sdio_host2::DataDirection::Read => DataDirection::Read,
                sdio_host2::DataDirection::Write => DataDirection::Write,
                _ => {
                    self.finish_host2_request(host2_id);
                    let data = sdio_host2::DataPhase {
                        direction: phase.direction,
                        block_size: phase.block_size,
                        block_count: phase.block_count,
                        buffer: sdio_host2::DataBuffer::Dma(buffer),
                    };
                    return Err(sdio_host2::SubmitTransactionError::new(
                        sdio_host2::Error::Unsupported,
                        sdio_host2::Transaction::with_data(transaction.command, data),
                    ));
                }
            },
        ) {
            self.finish_host2_request(host2_id);
            let tx = sdio_host2::Transaction::with_data(
                transaction.command,
                sdio_host2::DataPhase {
                    direction: phase.direction,
                    block_size: phase.block_size,
                    block_count: phase.block_count,
                    buffer: sdio_host2::DataBuffer::Dma(buffer),
                },
            );
            return Err(sdio_host2::SubmitTransactionError::new(
                sdio_host2::Error::Unsupported,
                tx,
            ));
        }
        let Some(dma) = self.dma.clone() else {
            self.finish_host2_request(host2_id);
            let data = sdio_host2::DataPhase {
                direction: phase.direction,
                block_size: phase.block_size,
                block_count: phase.block_count,
                buffer: sdio_host2::DataBuffer::Dma(buffer),
            };
            return Err(sdio_host2::SubmitTransactionError::new(
                sdio_host2::Error::Unsupported,
                sdio_host2::Transaction::with_data(transaction.command, data),
            ));
        };
        let mut slot = BlockRequestSlot::default();
        let submit = match phase.direction {
            sdio_host2::DataDirection::Read => self.submit_prepared_read_blocks(
                transaction.command.argument,
                buffer,
                &dma,
                &mut slot,
            ),
            sdio_host2::DataDirection::Write => self.submit_prepared_write_blocks(
                transaction.command.argument,
                buffer,
                &dma,
                &mut slot,
            ),
            _ => unreachable!("unsupported direction returned before submit"),
        };
        match submit {
            Ok(request) => {
                let id = request.id();
                let data = DataRequest {
                    id,
                    request: Some(request),
                    slot,
                    _buffer: PhantomData,
                };
                Ok(TransactionRequest::data(owner, host2_id, data, response))
            }
            Err(err) => {
                self.finish_host2_request(host2_id);
                let error = err.error;
                let buffer = err.into_buffer();
                let data = sdio_host2::DataPhase {
                    direction: phase.direction,
                    block_size: phase.block_size,
                    block_count: phase.block_count,
                    buffer: sdio_host2::DataBuffer::Dma(buffer),
                };
                Err(sdio_host2::SubmitTransactionError::new(
                    map_protocol_error(error),
                    sdio_host2::Transaction::with_data(transaction.command, data),
                ))
            }
        }
    }

    fn poll_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
    where
        Self: 'a,
    {
        self.check_host2_transaction_request(request)?;
        match request.kind {
            TransactionRequestKind::Command { response } => {
                match <Self as ProtocolSdioHost>::poll_command_response(self) {
                    Ok(sdmmc_protocol::CommandResponsePoll::Pending) => {
                        Ok(sdio_host2::RequestPoll::Pending)
                    }
                    Ok(sdmmc_protocol::CommandResponsePoll::Complete(resp)) => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdio_host2::RequestPoll::Ready(Ok(
                            resp.to_raw_response(response)
                        )))
                    }
                    Ok(_) => Ok(sdio_host2::RequestPoll::Pending),
                    Err(err) => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdio_host2::RequestPoll::Ready(Err(map_protocol_error(err))))
                    }
                }
            }
            TransactionRequestKind::Data { response } => {
                let Some(data) = request.data.as_mut() else {
                    let recovery = self.abort_host2_transaction_request(request).err();
                    return Ok(sdio_host2::RequestPoll::Ready(Err(
                        recovery.unwrap_or(sdio_host2::Error::InvalidArgument)
                    )));
                };
                match <Self as ProtocolSdioHost>::poll_data_request(self, data) {
                    Ok(DataCommandPoll::Pending) => Ok(sdio_host2::RequestPoll::Pending),
                    Ok(DataCommandPoll::Complete(resp)) => {
                        self.complete_host2_transaction_request(request);
                        Ok(sdio_host2::RequestPoll::Ready(Ok(
                            resp.to_raw_response(response)
                        )))
                    }
                    Ok(_) => Ok(sdio_host2::RequestPoll::Pending),
                    Err(err) => {
                        let _ = self.abort_host2_transaction_request(request);
                        Ok(sdio_host2::RequestPoll::Ready(Err(map_protocol_error(err))))
                    }
                }
            }
        }
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), sdio_host2::Error>
    where
        Self: 'a,
    {
        if request.done {
            return Ok(());
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::Error::InvalidArgument);
        }
        self.abort_host2_transaction_request(request)
    }

    fn take_completed_dma<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<dma_api::CompletedDma>
    where
        Self: 'a,
    {
        request
            .data
            .as_mut()
            .and_then(|data| data.slot.take_completed_dma())
    }

    unsafe fn submit_bus_op(
        &mut self,
        op: sdio_host2::BusOp,
    ) -> Result<Self::BusRequest, sdio_host2::Error> {
        self.check_not_poisoned().map_err(map_protocol_error)?;
        if !self.physical_bus_idle() {
            return Err(sdio_host2::Error::Busy);
        }
        let state = self.prepare_host2_bus_op(op)?;
        let owner = self.host2_owner();
        let id = self.start_host2_request();
        Ok(BusRequest::pending(owner, id, state))
    }

    fn poll_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError> {
        self.check_host2_bus_request(request)?;
        match self.poll_host2_bus_state(&mut request.state) {
            Ok(sdio_host2::RequestPoll::Pending) => Ok(sdio_host2::RequestPoll::Pending),
            Ok(sdio_host2::RequestPoll::Ready(Ok(()))) => {
                self.complete_host2_bus_request(request);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            Ok(sdio_host2::RequestPoll::Ready(Err(err))) => {
                let _ = self.abort_host2_bus_state(&mut request.state);
                self.complete_host2_bus_request(request);
                Ok(sdio_host2::RequestPoll::Ready(Err(err)))
            }
            Err(err) => {
                let _ = self.abort_host2_bus_state(&mut request.state);
                self.complete_host2_bus_request(request);
                Ok(sdio_host2::RequestPoll::Ready(Err(err)))
            }
        }
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), sdio_host2::Error> {
        if request.done {
            return Ok(());
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::Error::InvalidArgument);
        }
        let result = self.abort_host2_bus_state(&mut request.state);
        request.done = true;
        self.finish_host2_request(request.id);
        result
    }

    fn now_ms(&self) -> Option<u64> {
        self.timer.map(HostTimer::now_ms)
    }
}

impl Sdhci {
    fn physical_bus_idle(&self) -> bool {
        matches!(self.command_state, command::CommandState::Idle)
            && self.pending_data.is_none()
            && self.host2_active_id.is_none()
    }

    fn start_host2_request(&mut self) -> u64 {
        let id = self.host2_next_id;
        self.host2_next_id = self.host2_next_id.wrapping_add(1);
        self.host2_active_id = Some(id);
        id
    }

    fn host2_owner(&self) -> usize {
        self.base_addr
    }

    fn finish_host2_request(&mut self, id: u64) {
        if self.host2_active_id == Some(id) {
            self.host2_active_id = None;
        }
    }

    fn prepare_host2_bus_op(
        &self,
        op: sdio_host2::BusOp,
    ) -> Result<BusRequestState, sdio_host2::Error> {
        match op {
            sdio_host2::BusOp::ResetAll => Ok(BusRequestState::Reset {
                mask: RESET_ALL,
                phase: Phase::Init,
                was_irq_enabled: self.completion_irq_enabled(),
                started: false,
                polls: 0,
            }),
            sdio_host2::BusOp::ResetCommandLine => Ok(BusRequestState::Reset {
                mask: RESET_CMD,
                phase: Phase::CommandSend,
                was_irq_enabled: self.completion_irq_enabled(),
                started: false,
                polls: 0,
            }),
            sdio_host2::BusOp::ResetDataLine => Ok(BusRequestState::Reset {
                mask: RESET_DAT,
                phase: Phase::DataRead,
                was_irq_enabled: self.completion_irq_enabled(),
                started: false,
                polls: 0,
            }),
            sdio_host2::BusOp::PowerOn => Ok(BusRequestState::PowerOn),
            sdio_host2::BusOp::PowerOff => Ok(BusRequestState::PowerOff),
            sdio_host2::BusOp::SetClock(speed) => self.prepare_host2_clock(speed),
            sdio_host2::BusOp::SetClockHz(sdio_host2::ClockHz(hz)) => {
                if self.base_clock_hz() == 0 {
                    return Err(sdio_host2::Error::Controller);
                }
                Ok(BusRequestState::SetClock(SdhciClockState::Start {
                    target_hz: hz,
                    uhs_mode: None,
                    high_speed: None,
                }))
            }
            sdio_host2::BusOp::SetBusWidth(width) => match width {
                BusWidth::Bit1 | BusWidth::Bit4 | BusWidth::Bit8 => {
                    Ok(BusRequestState::SetBusWidth(width))
                }
                _ => Err(sdio_host2::Error::Unsupported),
            },
            sdio_host2::BusOp::SetSignalVoltage(voltage) => self.prepare_host2_voltage(voltage),
            sdio_host2::BusOp::ExecuteTuning {
                command,
                block_size,
            } => self.prepare_host2_tuning(command, block_size),
            _ => Err(sdio_host2::Error::Unsupported),
        }
    }

    fn prepare_host2_clock(&self, speed: ClockSpeed) -> Result<BusRequestState, sdio_host2::Error> {
        let (target_hz, uhs_mode) = match speed {
            ClockSpeed::Identification => (400_000, HOST_CTRL2_UHS_SDR12),
            ClockSpeed::Default | ClockSpeed::Sdr12 => (25_000_000, HOST_CTRL2_UHS_SDR12),
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => (50_000_000, HOST_CTRL2_UHS_SDR25),
            ClockSpeed::Sdr50 => (50_000_000, HOST_CTRL2_UHS_SDR50),
            ClockSpeed::Ddr50 => (50_000_000, HOST_CTRL2_UHS_DDR50),
            ClockSpeed::Sdr104 => (104_000_000, HOST_CTRL2_UHS_SDR104),
            ClockSpeed::Hs200 => (200_000_000, HOST_CTRL2_UHS_SDR104),
            _ => return Err(sdio_host2::Error::Unsupported),
        };
        if self.ext_clock.is_none() && self.base_clock_hz() == 0 {
            return Err(sdio_host2::Error::Controller);
        }
        let high_speed = !matches!(
            speed,
            ClockSpeed::Identification | ClockSpeed::Default | ClockSpeed::Sdr12
        );
        Ok(BusRequestState::SetClock(SdhciClockState::Start {
            target_hz,
            uhs_mode: Some(uhs_mode),
            high_speed: Some(high_speed),
        }))
    }

    fn prepare_host2_voltage(
        &self,
        voltage: SignalVoltage,
    ) -> Result<BusRequestState, sdio_host2::Error> {
        if matches!(voltage, SignalVoltage::V180) && !self.support_1v8 {
            return Err(sdio_host2::Error::Unsupported);
        }
        if matches!(voltage, SignalVoltage::V180) && self.timer.is_none() {
            return Err(sdio_host2::Error::Unsupported);
        }
        match voltage {
            SignalVoltage::V330 | SignalVoltage::V180 => Ok(BusRequestState::SetSignalVoltage(
                SdhciVoltageState::DisableClock(voltage),
            )),
            SignalVoltage::V120 => Err(sdio_host2::Error::Unsupported),
            _ => Err(sdio_host2::Error::Unsupported),
        }
    }

    fn prepare_host2_tuning(
        &self,
        command: sdio_host2::Command,
        block_size: core::num::NonZeroU16,
    ) -> Result<BusRequestState, sdio_host2::Error> {
        if command.index != 19 && command.index != 21 {
            return Err(sdio_host2::Error::InvalidArgument);
        }
        let expected =
            if command.index == 21 && self.read_u8(REG_HOST_CONTROL1) & HOST_CTRL1_8BIT != 0 {
                sdmmc_protocol::cmd::MMC_TUNING_BLOCK_SIZE_8BIT
            } else {
                sdmmc_protocol::cmd::SD_TUNING_BLOCK_SIZE
            };
        if u32::from(block_size.get()) != expected {
            return Err(sdio_host2::Error::InvalidArgument);
        }
        Ok(BusRequestState::ExecuteTuning(SdhciTuningState::Start {
            cmd_index: command.index,
            block_size: block_size.get(),
        }))
    }

    fn poll_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match state {
            BusRequestState::Reset {
                mask,
                phase,
                was_irq_enabled,
                started,
                polls,
            } => self.poll_host2_reset(*mask, *phase, *was_irq_enabled, started, polls),
            BusRequestState::PowerOn => {
                self.set_power(POWER_330);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::PowerOff => {
                self.write_u8(REG_POWER_CONTROL, 0);
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetClock(clock) => self.poll_host2_clock(clock),
            BusRequestState::SetBusWidth(width) => {
                self.apply_bus_width(*width).map_err(map_protocol_error)?;
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
            BusRequestState::SetSignalVoltage(voltage) => self.poll_host2_voltage(voltage),
            BusRequestState::ExecuteTuning(tuning) => self.poll_host2_tuning(tuning),
        }
    }

    fn poll_host2_reset(
        &mut self,
        mask: u8,
        phase: Phase,
        was_irq_enabled: bool,
        started: &mut bool,
        polls: &mut u32,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        if !*started {
            self.write_u8(REG_SOFTWARE_RESET, mask);
            *started = true;
        }
        if self.read_u8(REG_SOFTWARE_RESET) & mask == 0 {
            if mask == RESET_ALL {
                if let Some(hook) = self.reset_hook {
                    hook.after_reset(self).map_err(map_protocol_error)?;
                }
                self.restore_completion_irq_after_reset(was_irq_enabled);
            }
            return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
        }
        if *polls >= SDHCI_RESET_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(phase))));
        }
        *polls += 1;
        Ok(sdio_host2::RequestPoll::Pending)
    }

    fn poll_host2_clock(
        &mut self,
        state: &mut SdhciClockState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match *state {
            SdhciClockState::Start {
                target_hz,
                uhs_mode,
                high_speed,
            } => {
                if let Some(mode) = uhs_mode {
                    let ctrl2 =
                        (self.read_u16(REG_HOST_CONTROL2) & !HOST_CTRL2_UHS_MODE_MASK) | mode;
                    self.write_u16(REG_HOST_CONTROL2, ctrl2);
                }
                if let Some(enabled) = high_speed {
                    let mut ctrl = self.read_u8(REG_HOST_CONTROL1);
                    if enabled {
                        ctrl |= HOST_CTRL1_HIGH_SPEED;
                    } else {
                        ctrl &= !HOST_CTRL1_HIGH_SPEED;
                    }
                    self.write_u8(REG_HOST_CONTROL1, ctrl);
                }
                if self.ext_clock.is_some() {
                    self.disable_sd_clock();
                    *state = SdhciClockState::ExternalSetClock { target_hz };
                } else {
                    self.start_internal_clock(target_hz)?;
                    *state = SdhciClockState::InternalWaitStable { polls: 0 };
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciClockState::ExternalSetClock { target_hz } => {
                let clock = self.ext_clock.ok_or(sdio_host2::Error::Controller)?;
                clock.set_clock(target_hz).map_err(map_protocol_error)?;
                self.start_external_clock();
                *state = SdhciClockState::ExternalEnable { polls: 0 };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciClockState::ExternalEnable { ref mut polls }
            | SdhciClockState::InternalWaitStable { ref mut polls } => {
                self.poll_clock_stable(polls)
            }
        }
    }

    fn start_internal_clock(&mut self, target_hz: u32) -> Result<(), sdio_host2::Error> {
        self.write_u16(REG_CLOCK_CONTROL, 0);
        if target_hz == 0 {
            return Ok(());
        }
        let base_clock_hz = self.base_clock_hz();
        if base_clock_hz == 0 {
            return Err(sdio_host2::Error::Controller);
        }
        let div = sdhci_clock_divisor(base_clock_hz, target_hz);
        let clk_ctrl = ((div & 0xFF) << 8) | ((div & 0x300) >> 2) | CLOCK_INTERNAL_ENABLE;
        self.write_u16(REG_CLOCK_CONTROL, clk_ctrl);
        Ok(())
    }

    fn start_external_clock(&mut self) {
        self.write_u16(REG_CLOCK_CONTROL, 0);
        self.write_u16(REG_CLOCK_CONTROL, CLOCK_INTERNAL_ENABLE);
    }

    fn poll_clock_stable(
        &mut self,
        polls: &mut u32,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        let clock = self.read_u16(REG_CLOCK_CONTROL);
        if clock & CLOCK_INTERNAL_ENABLE == 0 {
            return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
        }
        if clock & CLOCK_INTERNAL_STABLE != 0 {
            self.write_u16(REG_CLOCK_CONTROL, clock | CLOCK_SD_ENABLE);
            return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
        }
        if *polls >= SDHCI_CLOCK_POLLS {
            return Err(map_protocol_error(Error::Timeout(ErrorContext::new(
                Phase::Init,
            ))));
        }
        *polls += 1;
        Ok(sdio_host2::RequestPoll::Pending)
    }

    fn poll_host2_voltage(
        &mut self,
        state: &mut SdhciVoltageState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match *state {
            SdhciVoltageState::DisableClock(voltage) => {
                self.disable_sd_clock();
                *state = SdhciVoltageState::SwitchControllerAndRail(voltage);
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciVoltageState::SwitchControllerAndRail(voltage) => {
                if matches!(voltage, SignalVoltage::V180) && !self.dat_3_0_lines_low() {
                    self.rollback_host2_voltage();
                    return Ok(sdio_host2::RequestPoll::Ready(Err(
                        sdio_host2::Error::Controller,
                    )));
                }
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
                    SignalVoltage::V120 => return Err(sdio_host2::Error::Unsupported),
                    _ => return Err(sdio_host2::Error::Unsupported),
                }
                self.write_u16(REG_HOST_CONTROL2, ctrl2);
                *state = SdhciVoltageState::WaitVsw {
                    voltage,
                    deadline_ms: self
                        .now_ms()
                        .map(|now| now.saturating_add(SDHCI_VOLTAGE_SWITCH_DELAY_MS)),
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciVoltageState::WaitVsw {
                voltage,
                deadline_ms,
            } => {
                if deadline_ms.is_none()
                    || deadline_ms
                        .zip(self.now_ms())
                        .is_some_and(|(deadline, now)| now >= deadline)
                {
                    *state = SdhciVoltageState::EnableClock(voltage);
                }
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciVoltageState::EnableClock(voltage) => {
                let cur = self.read_u16(REG_CLOCK_CONTROL);
                self.write_u16(REG_CLOCK_CONTROL, cur | CLOCK_SD_ENABLE);
                *state = SdhciVoltageState::VerifyDatLines(voltage);
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciVoltageState::VerifyDatLines(voltage) => {
                if matches!(voltage, SignalVoltage::V180) && !self.dat_3_0_lines_high() {
                    self.rollback_host2_voltage();
                    return Ok(sdio_host2::RequestPoll::Ready(Err(
                        sdio_host2::Error::Controller,
                    )));
                }
                Ok(sdio_host2::RequestPoll::Ready(Ok(())))
            }
        }
    }

    fn poll_host2_tuning(
        &mut self,
        state: &mut SdhciTuningState,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::Error> {
        match *state {
            SdhciTuningState::Start {
                cmd_index,
                block_size,
            } => {
                self.write_u16(REG_BLOCK_SIZE, block_size & 0x0FFF);
                self.write_u16(REG_BLOCK_COUNT, 1);
                self.write_u8(REG_TIMEOUT_CONTROL, 0x0E);
                self.write_u16(
                    REG_TRANSFER_MODE,
                    XFER_MODE_BLOCK_COUNT_ENABLE | XFER_MODE_READ,
                );
                let ctrl2 = self.read_u16(REG_HOST_CONTROL2) | HOST_CTRL2_EXECUTE_TUNING;
                self.write_u16(REG_HOST_CONTROL2, ctrl2);
                *state = SdhciTuningState::Wait {
                    cmd_index,
                    polls: 0,
                };
                Ok(sdio_host2::RequestPoll::Pending)
            }
            SdhciTuningState::Wait {
                cmd_index,
                ref mut polls,
            } => {
                let status = self.read_u16(REG_HOST_CONTROL2);
                if status & HOST_CTRL2_EXECUTE_TUNING == 0 {
                    if status & HOST_CTRL2_SAMPLING_CLOCK_SELECT != 0 {
                        return Ok(sdio_host2::RequestPoll::Ready(Ok(())));
                    }
                    return Err(map_protocol_error(Error::BadResponse(
                        ErrorContext::for_cmd(Phase::Init, cmd_index),
                    )));
                }
                if *polls >= SDHCI_TUNING_POLLS {
                    self.write_u16(REG_HOST_CONTROL2, status & !HOST_CTRL2_EXECUTE_TUNING);
                    return Err(map_protocol_error(Error::Timeout(ErrorContext::for_cmd(
                        Phase::Init,
                        cmd_index,
                    ))));
                }
                *polls += 1;
                Ok(sdio_host2::RequestPoll::Pending)
            }
        }
    }

    fn abort_host2_bus_state(
        &mut self,
        state: &mut BusRequestState,
    ) -> Result<(), sdio_host2::Error> {
        match state {
            BusRequestState::Reset { mask, started, .. } if *started => {
                if !self.reset_with_mask_best_effort(*mask) {
                    return Err(sdio_host2::Error::Timeout);
                }
            }
            BusRequestState::SetClock(_) => self.reset_controller_for_host2_abort()?,
            BusRequestState::SetSignalVoltage(_) => self.rollback_host2_voltage(),
            BusRequestState::ExecuteTuning(SdhciTuningState::Wait { .. }) => {
                let ctrl2 = self.read_u16(REG_HOST_CONTROL2) & !HOST_CTRL2_EXECUTE_TUNING;
                self.write_u16(REG_HOST_CONTROL2, ctrl2);
                self.reset_controller_for_host2_abort()?;
            }
            _ => {}
        }
        Ok(())
    }

    fn reset_controller_for_host2_abort(&mut self) -> Result<(), sdio_host2::Error> {
        let was_irq_enabled = self.completion_irq_enabled();
        self.write_u8(REG_SOFTWARE_RESET, RESET_ALL);
        if !self.reset_with_mask_best_effort(RESET_ALL) {
            return Err(sdio_host2::Error::Timeout);
        }
        if let Some(hook) = self.reset_hook {
            hook.after_reset(self).map_err(map_protocol_error)?;
        }
        self.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_CLEAR_ALL);
        self.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_CLEAR_ALL);
        self.clear_cached_irq_status();
        self.restore_completion_irq_after_reset(was_irq_enabled);
        self.pending_data = None;
        self.command_state = command::CommandState::Idle;
        Ok(())
    }

    fn restore_completion_irq_after_reset(&mut self, was_irq_enabled: bool) {
        self.enable_interrupts();
        if was_irq_enabled {
            self.enable_completion_irq();
        }
    }

    fn rollback_host2_voltage(&mut self) {
        self.disable_sd_clock();
        let ctrl2 = self.read_u16(REG_HOST_CONTROL2) & !HOST_CTRL2_1V8_SIGNALING;
        self.write_u16(REG_HOST_CONTROL2, ctrl2);
        self.set_power(POWER_330);
        let clock = self.read_u16(REG_CLOCK_CONTROL);
        self.write_u16(REG_CLOCK_CONTROL, clock | CLOCK_SD_ENABLE);
    }

    fn dat_3_0_lines_high(&self) -> bool {
        self.read_u32(REG_PRESENT_STATE) & PRESENT_DAT_3_0_LINE_SIGNAL_LEVEL
            == PRESENT_DAT_3_0_LINE_SIGNAL_LEVEL
    }

    fn dat_3_0_lines_low(&self) -> bool {
        self.read_u32(REG_PRESENT_STATE) & PRESENT_DAT_3_0_LINE_SIGNAL_LEVEL == 0
    }

    fn reset_with_mask_best_effort(&mut self, mask: u8) -> bool {
        for _ in 0..SDHCI_RESET_POLLS {
            if self.read_u8(REG_SOFTWARE_RESET) & mask == 0 {
                return true;
            }
            core::hint::spin_loop();
        }
        false
    }

    fn apply_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
        let mut ctrl = self.read_u8(REG_HOST_CONTROL1);
        ctrl &= !(HOST_CTRL1_4BIT | HOST_CTRL1_8BIT);
        match width {
            BusWidth::Bit1 => {}
            BusWidth::Bit4 => ctrl |= HOST_CTRL1_4BIT,
            BusWidth::Bit8 => ctrl |= HOST_CTRL1_8BIT,
            _ => return Err(Error::UnsupportedCommand),
        }
        self.write_u8(REG_HOST_CONTROL1, ctrl);
        Ok(())
    }

    fn check_host2_transaction_request(
        &self,
        request: &TransactionRequest<'_>,
    ) -> Result<(), sdio_host2::PollRequestError> {
        if request.done {
            return Err(sdio_host2::PollRequestError::AlreadyCompleted);
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::PollRequestError::WrongOwner);
        }
        if self.host2_active_id != Some(request.id) {
            return Err(sdio_host2::PollRequestError::StaleGeneration);
        }
        Ok(())
    }

    fn check_host2_bus_request(
        &self,
        request: &BusRequest,
    ) -> Result<(), sdio_host2::PollRequestError> {
        if request.done {
            return Err(sdio_host2::PollRequestError::AlreadyCompleted);
        }
        if request.owner != self.host2_owner() {
            return Err(sdio_host2::PollRequestError::WrongOwner);
        }
        if self.host2_active_id != Some(request.id) {
            return Err(sdio_host2::PollRequestError::StaleGeneration);
        }
        Ok(())
    }

    fn complete_host2_transaction_request(&mut self, request: &mut TransactionRequest<'_>) {
        request.done = true;
        self.finish_host2_request(request.id);
    }

    fn complete_host2_bus_request(&mut self, request: &mut BusRequest) {
        request.done = true;
        self.finish_host2_request(request.id);
    }

    fn abort_host2_transaction_request(
        &mut self,
        request: &mut TransactionRequest<'_>,
    ) -> Result<(), sdio_host2::Error> {
        let result = if let Some(data) = request.data.as_mut() {
            if let Some(active) = data.request.take() {
                let id = active.id();
                let mut pending = Some(active);
                self.abort_block_request_response(&mut pending, id, &mut data.slot)
                    .map_err(map_protocol_error)
            } else {
                Ok(())
            }
        } else {
            self.abort_command().map_err(map_protocol_error)
        };
        request.done = true;
        self.finish_host2_request(request.id);
        result
    }
}

fn map_protocol_error(err: Error) -> sdio_host2::Error {
    match err {
        Error::Timeout(_) => sdio_host2::Error::Timeout,
        Error::Crc(_) => sdio_host2::Error::Crc,
        Error::NoCard => sdio_host2::Error::NoCard,
        Error::Busy => sdio_host2::Error::Busy,
        Error::UnsupportedCommand => sdio_host2::Error::Unsupported,
        Error::Misaligned => sdio_host2::Error::Misaligned,
        Error::InvalidArgument => sdio_host2::Error::InvalidArgument,
        Error::BusError(_) => sdio_host2::Error::Bus,
        Error::ReadError(_) | Error::WriteError(_) | Error::BadResponse(_) => {
            sdio_host2::Error::Bus
        }
        Error::CardError(_) | Error::CardLocked => sdio_host2::Error::Controller,
        _ => sdio_host2::Error::Controller,
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

fn submit_read_with_dma_fifo_fallback(
    host: &mut Sdhci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    if should_try_dma(cmd, block_size, block_count, len, DataDirection::Read)
        && let Some(dma) = host.dma.clone()
    {
        match host.submit_read_blocks(
            cmd.argument,
            buffer,
            NonZeroUsize::new(len).ok_or(Error::InvalidArgument)?,
            Some(&dma),
            BlockTransferMode::Dma,
            slot,
        ) {
            Ok(request) => {
                log_adma_path_once("read");
                return Ok(request);
            }
            Err(err) if can_fallback_to_fifo(err) => {
                log_adma_fallback_once("read", err);
            }
            Err(err) => return Err(err),
        }
    }

    host.submit_fifo_data_request(
        cmd,
        buffer,
        len,
        block_size,
        block_count,
        DataDirection::Read,
        slot,
    )
}

fn submit_write_with_dma_fifo_fallback(
    host: &mut Sdhci,
    cmd: &Command,
    buffer: NonNull<u8>,
    len: usize,
    block_size: u32,
    block_count: u32,
    slot: &mut BlockRequestSlot,
) -> Result<BlockRequest, Error> {
    if should_try_dma(cmd, block_size, block_count, len, DataDirection::Write)
        && let Some(dma) = host.dma.clone()
    {
        match host.submit_write_blocks(
            cmd.argument,
            buffer,
            NonZeroUsize::new(len).ok_or(Error::InvalidArgument)?,
            Some(&dma),
            BlockTransferMode::Dma,
            slot,
        ) {
            Ok(request) => {
                log_adma_path_once("write");
                return Ok(request);
            }
            Err(err) if can_fallback_to_fifo(err) => {
                log_adma_fallback_once("write", err);
            }
            Err(err) => return Err(err),
        }
    }

    host.submit_fifo_data_request(
        cmd,
        buffer,
        len,
        block_size,
        block_count,
        DataDirection::Write,
        slot,
    )
}

fn should_try_dma(
    cmd: &Command,
    block_size: u32,
    block_count: u32,
    len: usize,
    direction: DataDirection,
) -> bool {
    block_size == 512
        && len == block_count as usize * 512
        && matches!(
            (direction, cmd.index),
            (DataDirection::Read, 17 | 18) | (DataDirection::Write, 24 | 25)
        )
}

fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}

fn log_adma_path_once(direction: &str) {
    let logged = match direction {
        "read" => &ADMA_READ_PATH_LOGGED,
        "write" => &ADMA_WRITE_PATH_LOGGED,
        _ => return,
    };
    if !logged.swap(true, Ordering::Relaxed) {
        log::info!("sdhci: using ADMA2 {direction} data path");
    }
}

fn log_adma_fallback_once(direction: &str, err: Error) {
    let logged = match direction {
        "read" => &ADMA_READ_FALLBACK_LOGGED,
        "write" => &ADMA_WRITE_FALLBACK_LOGGED,
        _ => return,
    };
    if !logged.swap(true, Ordering::Relaxed) {
        log::warn!("sdhci: falling back to FIFO for {direction} data path: {err:?}");
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

    pub fn irq_handle(&self) -> SdhciIrqHandle {
        SdhciIrqHandle {
            irq: self.irq.clone(),
        }
    }

    /// Read and acknowledge pending controller status, returning a stable
    /// event for OS glue to translate into wakeups or worker scheduling.
    pub fn handle_irq(&self) -> Event {
        self.irq_handle().handle_irq()
    }
}

impl SdioIrqHandle for SdhciIrqHandle {
    type Event = Event;

    fn handle_irq(&self) -> Self::Event {
        let generation = self.irq.state.generation();
        let normal = read_u16(self.irq.base_addr, REG_NORMAL_INT_STATUS);
        let error = if normal & NORMAL_INT_ERROR != 0 {
            read_u16(self.irq.base_addr, REG_ERROR_INT_STATUS)
        } else {
            0
        };

        if normal != 0 {
            write_u16(self.irq.base_addr, REG_NORMAL_INT_STATUS, normal);
        }
        if error != 0 {
            write_u16(self.irq.base_addr, REG_ERROR_INT_STATUS, error);
        }
        self.irq.state.cache_if_current(generation, normal, error);

        event_from_status(normal, error)
    }
}

fn read_u16(base_addr: usize, off: usize) -> u16 {
    unsafe { core::ptr::read_volatile((base_addr + off) as *const u16) }
}

fn write_u16(base_addr: usize, off: usize, val: u16) {
    unsafe { core::ptr::write_volatile((base_addr + off) as *mut u16, val) }
}

#[cfg(test)]
mod tests {
    use core::num::{NonZeroU16, NonZeroU32};

    use sdio_host2::ResponseType;

    use super::*;

    #[test]
    fn event_reports_command_completion_without_os_wakeup_policy() {
        assert_eq!(
            event_from_status(NORMAL_INT_CMD_COMPLETE, 0),
            Event::CommandComplete
        );
    }

    #[test]
    fn event_reports_data_completion_without_os_wakeup_policy() {
        assert_eq!(
            event_from_status(NORMAL_INT_XFER_COMPLETE, 0),
            Event::TransferComplete
        );
    }

    #[test]
    fn event_reports_error_status_without_translating_to_os_action() {
        assert_eq!(
            event_from_status(NORMAL_INT_ERROR, ERROR_INT_DATA_TIMEOUT),
            Event::Error {
                normal: NORMAL_INT_ERROR,
                error: ERROR_INT_DATA_TIMEOUT,
            }
        );
    }

    #[test]
    fn event_reports_data_completion_source_for_runtime_wakeup() {
        use sdmmc_protocol::sdio::{HostEvent, HostEventKind, HostEventSource};

        let event = event_from_status(NORMAL_INT_XFER_COMPLETE, 0);

        assert_eq!(event.kind(), HostEventKind::TransferComplete);
        assert_eq!(event.source(), HostEventSource::Data);
        assert_eq!(event.queue_id(), Some(BlockRequestId::new(0)));
    }

    #[test]
    fn merged_command_and_data_irq_reports_queue_ready() {
        use sdmmc_protocol::sdio::{HostEvent, HostEventKind, HostEventSource};

        let event = event_from_status(NORMAL_INT_CMD_COMPLETE | NORMAL_INT_XFER_COMPLETE, 0);

        assert_eq!(event.kind(), HostEventKind::TransferComplete);
        assert_eq!(event.source(), HostEventSource::Data);
        assert_eq!(event.queue_id(), Some(BlockRequestId::new(0)));
    }

    #[test]
    fn exposes_block_buffer_constraints() {
        let host = unsafe { Sdhci::new_from_addr(0x1000_0000) };

        let dma = host.block_buffer_config(BlockTransferMode::Dma);
        assert_eq!(dma.block_size.get(), 512);
        assert_eq!(dma.align, 512);
        assert_eq!(dma.dma_mask, Some(u32::MAX as u64));
    }

    #[test]
    fn host2_data_submit_reports_busy_without_dirtying_pending_data() {
        let mut host = unsafe { Sdhci::new_from_addr(0x1000_0000) };
        host.command_state = command::CommandState::Issued {
            cmd: Command::new(0, 0, ResponseType::None),
            data_line: false,
            polls: 0,
        };
        let mut buf = [0u8; 512];
        let data = sdio_host2::DataPhase::read(
            NonZeroU16::new(512).unwrap(),
            NonZeroU32::new(1).unwrap(),
            &mut buf,
        )
        .unwrap();
        let tx = sdio_host2::Transaction::with_data(Command::new(17, 0, ResponseType::R1), data);

        let err =
            match unsafe { <Sdhci as sdio_host2::SdioHost>::submit_transaction(&mut host, tx) } {
                Ok(_) => panic!("busy host accepted a second transaction"),
                Err(err) => err,
            };

        assert_eq!(err, sdio_host2::Error::Busy);
        assert!(host.pending_data.is_none());
    }

    #[test]
    fn host2_poll_after_complete_is_rejected() {
        #[repr(align(4))]
        struct FakeRegs([u8; 0x100]);

        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        let mut request = unsafe {
            <Sdhci as sdio_host2::SdioHost>::submit_bus_op(&mut host, sdio_host2::BusOp::PowerOn)
        }
        .unwrap();

        assert!(matches!(
            <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
            Ok(sdio_host2::RequestPoll::Ready(Ok(())))
        ));
        assert_eq!(
            <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
            Err(sdio_host2::PollRequestError::AlreadyCompleted)
        );
    }

    #[test]
    fn host2_bus_request_is_bound_to_originating_host() {
        #[repr(align(4))]
        struct FakeRegs([u8; 0x100]);

        let mut regs_a = FakeRegs([0; 0x100]);
        let mut regs_b = FakeRegs([0; 0x100]);
        let base_a = NonNull::new(regs_a.0.as_mut_ptr()).unwrap();
        let base_b = NonNull::new(regs_b.0.as_mut_ptr()).unwrap();
        let mut host_a = unsafe { Sdhci::new(base_a) };
        let mut host_b = unsafe { Sdhci::new(base_b) };
        let mut request = unsafe {
            <Sdhci as sdio_host2::SdioHost>::submit_bus_op(&mut host_a, sdio_host2::BusOp::PowerOn)
        }
        .unwrap();

        assert_eq!(
            <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host_b, &mut request),
            Err(sdio_host2::PollRequestError::WrongOwner)
        );
    }

    #[test]
    fn host2_v180_requires_real_timer() {
        let mut host = unsafe { Sdhci::new_from_addr(0x1000_0000) };
        host.enable_1v8_signaling();

        assert!(matches!(
            unsafe {
                <Sdhci as sdio_host2::SdioHost>::submit_bus_op(
                    &mut host,
                    sdio_host2::BusOp::SetSignalVoltage(sdio_host2::SignalVoltage::V180),
                )
            },
            Err(sdio_host2::Error::Unsupported)
        ));
    }

    #[test]
    fn host2_v180_rejects_partial_high_dat_lines_before_switch() {
        #[repr(align(4))]
        struct FakeRegs([u8; 0x100]);

        struct StaticTimer;

        impl HostTimer for StaticTimer {
            fn now_ms(&self) -> u64 {
                0
            }
        }

        static TIMER: StaticTimer = StaticTimer;

        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        host.enable_1v8_signaling();
        host.set_timer(&TIMER);
        host.write_u32(REG_PRESENT_STATE, 1 << 20);
        let mut request = unsafe {
            <Sdhci as sdio_host2::SdioHost>::submit_bus_op(
                &mut host,
                sdio_host2::BusOp::SetSignalVoltage(sdio_host2::SignalVoltage::V180),
            )
        }
        .unwrap();

        assert!(matches!(
            <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
            Ok(sdio_host2::RequestPoll::Pending)
        ));
        assert!(matches!(
            <Sdhci as sdio_host2::SdioHost>::poll_bus_op(&mut host, &mut request),
            Ok(sdio_host2::RequestPoll::Ready(Err(
                sdio_host2::Error::Controller
            )))
        ));
    }

    #[test]
    fn irq_handle_acks_and_caches_status_without_mutable_host() {
        #[repr(align(4))]
        struct FakeRegs([u8; 0x100]);

        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let host = unsafe { Sdhci::new(base) };
        host.irq.state.begin_request();
        host.write_u16(REG_NORMAL_INT_STATUS, NORMAL_INT_ERROR);
        host.write_u16(REG_ERROR_INT_STATUS, ERROR_INT_DATA_TIMEOUT);

        let handle = host.irq_handle().clone();

        assert_eq!(
            handle.handle_irq(),
            Event::Error {
                normal: NORMAL_INT_ERROR,
                error: ERROR_INT_DATA_TIMEOUT,
            }
        );
        assert_eq!(host.irq.state.pending_normal(), NORMAL_INT_ERROR);
        assert_eq!(host.irq.state.pending_error(), ERROR_INT_DATA_TIMEOUT);
        host.write_u16(REG_NORMAL_INT_STATUS, 0);
        host.write_u16(REG_ERROR_INT_STATUS, 0);
        assert_eq!(host.handle_irq(), Event::None);
    }
}
