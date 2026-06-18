//! Synopsys DesignWare Mobile Storage Host Controller (DW_mshc) backend
//! for the [`sdmmc-protocol`](sdmmc_protocol) driver crate.
//!
//! Implements [`sdmmc_protocol::sdio::SdioHost`] for the IP block known
//! variously as DWC_mobile_storage, dw_mshc, dw_mmc (Linux), or simply
//! the "Synopsys SD/MMC controller" — the same core used in Rockchip
//! RK33xx/RK35xx, Allwinner A-series, StarFive JH7110, and a long
//! tail of mid-range SoCs. Block I/O can be submitted through either the
//! FIFO path or the internal DMAC (IDMAC) path with the same poll contract.
//!
//! # Scope
//!
//! - **Implemented**: PIO data transfer over the 0x100/0x200/0x400
//!   FIFO (configurable), IDMAC descriptor transfers,
//!   1-bit / 4-bit / 8-bit bus selection,
//!   default / high-speed / UHS-I / HS200 clocking, DW_mshc UHS DDR
//!   and 1.8 V signaling bits, R1/R1b/R2/R3/R4/R5/R6/R7 response
//!   decoding, software reset.
//! - **Out of scope (for now)**: external-DMA path, controller-specific
//!   DLL/strobe/tuning window setup (CMD19/CMD21).
//!
//! # Usage
//!
//! ```rust,no_run
//! use core::ptr::NonNull;
//!
//! use dwmmc_host::DwMmc;
//! use sdmmc_protocol::sdio::{SdioInitScratch, SdioSdmmc};
//!
//! // SAFETY: 0xFE2B_0000 must point at a valid DW_mshc register file
//! // the caller has exclusive access to.
//! let mmio = NonNull::new(0xFE2B_0000 as *mut u8).unwrap();
//! let mut host = unsafe { DwMmc::new(mmio) };
//! host.set_reference_clock(50_000_000);
//! host.reset_and_init().expect("controller reset");
//!
//! let mut card = SdioSdmmc::new(host);
//! let mut scratch = SdioInitScratch::new();
//! let mut request = card.submit_init(&mut scratch)?;
//! // Poll request here. Runtime code chooses spin, yield, IRQ wait, or timer.
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! The runtime block queue adapter belongs in OS/platform glue. The reusable
//! driver crate exposes request state and host submit/poll primitives instead:
//!
//! ```compile_fail
//! use dwmmc_host::BlockQueue;
//! ```
//!
//! Construction is `unsafe` because the caller must guarantee that
//! the supplied address is a valid, exclusively-owned DW_mshc
//! register file.

#![no_std]
#![allow(clippy::missing_safety_doc)]

use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

mod command;
mod dma;
mod host;
mod regs;

pub use sdmmc_protocol::block::{
    BlockBufferConfig, BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode,
    BlockTransferState,
};
use sdmmc_protocol::{
    DataCommandPoll,
    cmd::{Command, DataDirection},
    error::Error,
    sdio::{
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, SdioHost, SdioIrqHandle,
        SdioIrqHost, SignalVoltage,
    },
};

use crate::regs::RegisterBlockVolatileFieldAccess;
pub use crate::{
    dma::{BlockRequest, BlockRequestSlot, IDMAC_DESC_ALIGN, IDMAC_DESC_SIZE, RequestId},
    host::{DEFAULT_FIFO_OFFSET, DwMmc},
};

/// Stable controller event extracted from DW_mshc raw interrupt status.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
    #[default]
    None,
    /// A command response has completed.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// Receive FIFO can be drained.
    ReceiveReady,
    /// Transmit FIFO can accept more data.
    TransmitReady,
    /// One or more controller error bits are pending.
    Error { raw_status: u32 },
    /// Status bits are pending but do not map to a high-level event yet.
    Other { raw_status: u32 },
}

pub struct DataRequest<'a> {
    id: RequestId,
    request: Option<BlockRequest>,
    slot: BlockRequestSlot,
    _buffer: PhantomData<&'a [u8]>,
}

/// Cloneable, sync-safe DWMMC IRQ top-half handle.
#[derive(Clone)]
pub struct DwMmcIrqHandle {
    regs: volatile::VolatilePtr<'static, crate::regs::RegisterBlock>,
    irq_state: *const host::IrqState,
}

// SAFETY: The handle only performs volatile MMIO accesses and atomic cache
// updates. The owning `DwMmc` outlives handles created by OS glue.
unsafe impl Send for DwMmcIrqHandle {}
// SAFETY: See the `Send` impl.
unsafe impl Sync for DwMmcIrqHandle {}

pub(crate) const DWMMC_INT_RESPONSE_ERROR: u32 = 1 << 1;
pub(crate) const DWMMC_INT_COMMAND_DONE: u32 = 1 << 2;
pub(crate) const DWMMC_INT_DATA_TRANSFER_OVER: u32 = 1 << 3;
pub(crate) const DWMMC_INT_TXDR: u32 = 1 << 4;
pub(crate) const DWMMC_INT_RXDR: u32 = 1 << 5;
pub(crate) const DWMMC_INT_RESPONSE_CRC_ERROR: u32 = 1 << 6;
pub(crate) const DWMMC_INT_DATA_CRC_ERROR: u32 = 1 << 7;
pub(crate) const DWMMC_INT_RESPONSE_TIMEOUT: u32 = 1 << 8;
pub(crate) const DWMMC_INT_DATA_READ_TIMEOUT: u32 = 1 << 9;
pub(crate) const DWMMC_INT_HOST_TIMEOUT: u32 = 1 << 10;
pub(crate) const DWMMC_INT_FIFO_UNDER_OVER_RUN: u32 = 1 << 11;
pub(crate) const DWMMC_INT_HARDWARE_LOCKED_WRITE: u32 = 1 << 12;
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

impl SdioHost for DwMmc {
    type Event = Event;
    type DataRequest<'a> = DataRequest<'a>;

    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        DwMmc::submit_command(self, cmd)
    }

    fn poll_command_response(&mut self) -> Result<sdmmc_protocol::CommandResponsePoll, Error> {
        DwMmc::poll_command_response(self)
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
        self.set_card_type(width);
        Ok(())
    }

    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
        let target_hz = clock_hz_for_speed(speed);
        self.set_uhs_timing(speed);
        self.program_clock(target_hz)
    }

    fn switch_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        self.set_signal_voltage(voltage)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        DwMmc::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        DwMmc::disable_completion_irq(self);
        Ok(())
    }

    fn handle_irq(&mut self) -> Self::Event {
        self.irq_handle().handle_irq()
    }
}

impl SdioIrqHost for DwMmc {
    type IrqHandle = DwMmcIrqHandle;

    fn irq_handle(&self) -> Self::IrqHandle {
        DwMmc::irq_handle(self)
    }

    fn completion_irq_enabled(&self) -> bool {
        DwMmc::completion_irq_enabled(self)
    }
}

fn submit_read_with_dma_fifo_fallback(
    host: &mut DwMmc,
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
            cmd.arg,
            buffer,
            NonZeroUsize::new(len).ok_or(Error::InvalidArgument)?,
            Some(&dma),
            BlockTransferMode::Dma,
            slot,
        ) {
            Ok(request) => return Ok(request),
            Err(err) if can_fallback_to_fifo(err) => {}
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
    host: &mut DwMmc,
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
            cmd.arg,
            buffer,
            NonZeroUsize::new(len).ok_or(Error::InvalidArgument)?,
            Some(&dma),
            BlockTransferMode::Dma,
            slot,
        ) {
            Ok(request) => return Ok(request),
            Err(err) if can_fallback_to_fifo(err) => {}
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
            (direction, cmd.cmd),
            (DataDirection::Read, 17 | 18) | (DataDirection::Write, 24 | 25)
        )
}

fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}

pub(crate) fn event_from_raw_status(raw_status: u32) -> Event {
    let status = crate::regs::RIntSts::from_bits(raw_status);
    if raw_status == 0 {
        Event::None
    } else if status.error() {
        Event::Error { raw_status }
    } else if status.command_done() {
        Event::CommandComplete
    } else if status.data_transfer_over() {
        Event::TransferComplete
    } else if status.receive_fifo_data_request() {
        Event::ReceiveReady
    } else if status.transmit_fifo_data_request() {
        Event::TransmitReady
    } else {
        Event::Other { raw_status }
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

impl DwMmc {
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

    pub fn irq_handle(&self) -> DwMmcIrqHandle {
        DwMmcIrqHandle {
            regs: self.regs,
            irq_state: &self.irq_state,
        }
    }

    /// Read and acknowledge pending controller status, returning a stable
    /// event for OS glue to translate into wakeups or worker scheduling.
    pub fn handle_irq(&self) -> Event {
        self.irq_handle().handle_irq()
    }
}

impl SdioIrqHandle for DwMmcIrqHandle {
    type Event = Event;

    fn handle_irq(&self) -> Self::Event {
        let raw_status = self.regs.mintsts().read();
        if raw_status != 0 {
            self.regs
                .rintsts()
                .write(crate::regs::RIntSts::from_bits(raw_status));
        }
        unsafe { &*self.irq_state }.cache(raw_status);
        event_from_raw_status(raw_status)
    }
}

fn clock_hz_for_speed(speed: ClockSpeed) -> u32 {
    match speed {
        ClockSpeed::Identification => 400_000,
        ClockSpeed::Default | ClockSpeed::Sdr12 => 25_000_000,
        ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => 50_000_000,
        ClockSpeed::Sdr50 | ClockSpeed::Ddr50 => 50_000_000,
        ClockSpeed::Sdr104 => 104_000_000,
        ClockSpeed::Hs200 => 200_000_000,
        // Future ClockSpeed variants: unknown frequency, signal 0.
        _ => 0,
    }
}

pub(crate) fn ddr_mask_for_speed(speed: ClockSpeed) -> u16 {
    match speed {
        ClockSpeed::Ddr50 => 1,
        _ => 0,
    }
}

pub(crate) fn volt_mask_for_signal(voltage: SignalVoltage) -> Result<u16, Error> {
    match voltage {
        SignalVoltage::V330 => Ok(0),
        SignalVoltage::V180 => Ok(1),
        SignalVoltage::V120 => Err(Error::UnsupportedCommand),
        // Future SignalVoltage variants are not supported by this controller.
        _ => Err(Error::UnsupportedCommand),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct UhsBits {
    pub ddr: u16,
    pub volt: u16,
}

pub(crate) fn uhs_bits_after_speed(cur: UhsBits, speed: ClockSpeed) -> UhsBits {
    UhsBits {
        ddr: ddr_mask_for_speed(speed),
        ..cur
    }
}

pub(crate) fn uhs_bits_after_voltage(
    cur: UhsBits,
    voltage: SignalVoltage,
) -> Result<UhsBits, Error> {
    Ok(UhsBits {
        volt: volt_mask_for_signal(voltage)?,
        ..cur
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_reports_command_completion_without_os_wakeup_policy() {
        let raw = crate::regs::RIntSts::new()
            .with_command_done(true)
            .into_bits();

        assert_eq!(event_from_raw_status(raw), Event::CommandComplete);
    }

    #[test]
    fn event_reports_transfer_completion_without_os_wakeup_policy() {
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();

        assert_eq!(event_from_raw_status(raw), Event::TransferComplete);
    }

    #[test]
    fn event_reports_error_status_without_translating_to_os_action() {
        let raw = crate::regs::RIntSts::new()
            .with_response_timeout(true)
            .into_bits();

        assert_eq!(event_from_raw_status(raw), Event::Error { raw_status: raw });
    }

    #[test]
    fn event_reports_data_completion_source_for_runtime_wakeup() {
        use sdmmc_protocol::sdio::{HostEvent, HostEventKind, HostEventSource};

        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();
        let event = event_from_raw_status(raw);

        assert_eq!(event.kind(), HostEventKind::TransferComplete);
        assert_eq!(event.source(), HostEventSource::Data);
        assert_eq!(event.queue_id(), Some(BlockRequestId::new(0)));
    }

    #[test]
    fn exposes_block_buffer_constraints() {
        let host = unsafe { DwMmc::new_from_addr(0x1000_0000) };

        let dma = host.block_buffer_config(BlockTransferMode::Dma);
        assert_eq!(dma.block_size.get(), 512);
        assert_eq!(dma.align, 512);
        assert_eq!(dma.dma_mask, Some(u32::MAX as u64));
    }

    #[test]
    fn irq_handle_acks_and_caches_status_without_mutable_host() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let host = unsafe { DwMmc::new(base) };
        let raw = crate::regs::RIntSts::new()
            .with_data_transfer_over(true)
            .into_bits();
        const MINTSTS_WORD: usize = 16;
        unsafe {
            mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(raw);
        }

        let handle = host.irq_handle().clone();

        assert_eq!(handle.handle_irq(), Event::TransferComplete);
        assert_eq!(host.irq_state.pending(), raw);
        unsafe {
            mmio.as_mut_ptr().add(MINTSTS_WORD).write_volatile(0);
        }
        assert_eq!(host.handle_irq(), Event::None);
    }

    #[test]
    fn uhs_i_sdr_modes_keep_ddr_disabled() {
        let cur = UhsBits { ddr: 1, volt: 1 };

        assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Sdr50).ddr, 0);
        assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Sdr104).ddr, 0);
        assert_eq!(uhs_bits_after_speed(cur, ClockSpeed::Hs200).ddr, 0);
    }

    #[test]
    fn ddr50_enables_ddr_mode_for_card0() {
        let cur = UhsBits { ddr: 0, volt: 1 };

        assert_eq!(
            uhs_bits_after_speed(cur, ClockSpeed::Ddr50),
            UhsBits { ddr: 1, volt: 1 }
        );
    }

    #[test]
    fn uhs_i_voltage_switch_selects_1v8_for_card0() {
        let cur = UhsBits { ddr: 1, volt: 0 };

        assert_eq!(
            uhs_bits_after_voltage(cur, SignalVoltage::V180).unwrap(),
            UhsBits { ddr: 1, volt: 1 }
        );
        assert_eq!(
            uhs_bits_after_voltage(cur, SignalVoltage::V330).unwrap(),
            UhsBits { ddr: 1, volt: 0 }
        );
    }

    #[test]
    fn unsupported_1v2_voltage_is_rejected() {
        assert_eq!(
            volt_mask_for_signal(SignalVoltage::V120).unwrap_err(),
            Error::UnsupportedCommand
        );
    }

    #[test]
    fn data_command_index_is_recorded_for_diagnostics() {
        let mut host = unsafe { DwMmc::new_from_addr(0x1000_0000) };
        host.data_cmd_index = 6;

        assert_eq!(host.data_cmd_index, 6);
    }
}
