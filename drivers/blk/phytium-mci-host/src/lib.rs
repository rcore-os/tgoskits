//! Phytium MCI/FSDIF host controller backend for `sdmmc-protocol`.
//!
//! The register layout is the Phytium Memory Card Interface found on E2000
//! class SoCs. It is close to the DesignWare MSHC programming model, with
//! Phytium-specific clock-source and timing registers.
//!
//! # Scope
//!
//! - **Implemented**: controller/FIFO reset, power and clock setup, Phytium
//!   timing tables, 1-bit / 4-bit / 8-bit bus selection, command response
//!   decoding, FIFO block transfers, and stable IRQ event extraction.
//! - **Out of scope for this crate**: FDT/ACPI probe, MMIO remapping, IRQ
//!   registration, pad-controller programming, OS sleeps/wakeups, and rdif-block
//!   registration.
//! - **Implemented for block I/O**: IDMAC descriptor setup, DMA buffer mapping,
//!   DMA block read/write polling, and FIFO fallback at the platform adapter.

#![no_std]
#![allow(clippy::missing_safety_doc)]

extern crate alloc;

use core::{marker::PhantomData, ptr::NonNull};

mod command;
mod dma;
mod host;
pub mod rdif;
mod regs;
mod timing;

pub use dma::{BlockRequest, BlockRequestSlot, RequestId};
pub use host::{DEFAULT_FIFO_OFFSET, PhytiumMci};
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

/// Stable controller event extracted from Phytium MCI raw interrupt status.
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
}

pub(crate) const MCI_INT_RESPONSE_ERROR: u32 = 1 << 1;
pub(crate) const MCI_INT_COMMAND_DONE: u32 = 1 << 2;
pub(crate) const MCI_INT_DATA_TRANSFER_OVER: u32 = 1 << 3;
pub(crate) const MCI_INT_TXDR: u32 = 1 << 4;
pub(crate) const MCI_INT_RXDR: u32 = 1 << 5;
pub(crate) const MCI_INT_RESPONSE_CRC_ERROR: u32 = 1 << 6;
pub(crate) const MCI_INT_DATA_CRC_ERROR: u32 = 1 << 7;
pub(crate) const MCI_INT_RESPONSE_TIMEOUT: u32 = 1 << 8;
pub(crate) const MCI_INT_DATA_READ_TIMEOUT: u32 = 1 << 9;
pub(crate) const MCI_INT_HOST_TIMEOUT: u32 = 1 << 10;
pub(crate) const MCI_INT_FIFO_UNDER_OVER_RUN: u32 = 1 << 11;
pub(crate) const MCI_INT_HARDWARE_LOCKED_WRITE: u32 = 1 << 12;
pub(crate) const MCI_INT_START_BIT_ERROR: u32 = 1 << 13;
pub(crate) const MCI_INT_END_BIT_ERROR: u32 = 1 << 15;
pub(crate) const MCI_INT_ERROR_MASK: u32 = MCI_INT_RESPONSE_ERROR
    | MCI_INT_RESPONSE_CRC_ERROR
    | MCI_INT_DATA_CRC_ERROR
    | MCI_INT_RESPONSE_TIMEOUT
    | MCI_INT_DATA_READ_TIMEOUT
    | MCI_INT_HOST_TIMEOUT
    | MCI_INT_FIFO_UNDER_OVER_RUN
    | MCI_INT_HARDWARE_LOCKED_WRITE
    | MCI_INT_START_BIT_ERROR
    | MCI_INT_END_BIT_ERROR;

pub(crate) const MCI_IDSTS_TRANSMIT: u32 = 1 << 0;
pub(crate) const MCI_IDSTS_RECEIVE: u32 = 1 << 1;
pub(crate) const MCI_IDSTS_FATAL_BUS_ERROR: u32 = 1 << 2;
pub(crate) const MCI_IDSTS_DESCRIPTOR_UNAVAILABLE: u32 = (1 << 3) | (1 << 4);
pub(crate) const MCI_IDSTS_CARD_ERROR_SUMMARY: u32 = 1 << 5;
pub(crate) const MCI_IDSTS_ERROR_MASK: u32 =
    MCI_IDSTS_FATAL_BUS_ERROR | MCI_IDSTS_DESCRIPTOR_UNAVAILABLE | MCI_IDSTS_CARD_ERROR_SUMMARY;

pub struct DataRequest<'a> {
    id: RequestId,
    request: Option<BlockRequest>,
    slot: BlockRequestSlot,
    _buffer: PhantomData<&'a [u8]>,
}

/// Cloneable, sync-safe Phytium MCI IRQ top-half handle.
#[derive(Clone)]
pub struct PhytiumMciIrqHandle {
    pub(crate) regs: volatile::VolatilePtr<'static, crate::regs::RegisterBlock>,
    pub(crate) irq_state: *const host::IrqState,
}

// SAFETY: The handle only performs volatile MMIO accesses and atomic cache
// updates. The owning `PhytiumMci` outlives handles created by OS glue.
unsafe impl Send for PhytiumMciIrqHandle {}
// SAFETY: See the `Send` impl.
unsafe impl Sync for PhytiumMciIrqHandle {}

impl SdioHost for PhytiumMci {
    type Event = Event;
    type DataRequest<'a> = DataRequest<'a>;

    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        PhytiumMci::submit_command(self, cmd)
    }

    fn poll_command_response(&mut self) -> Result<sdmmc_protocol::CommandResponsePoll, Error> {
        PhytiumMci::poll_command_response(self)
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
        let request = self.submit_fifo_data_request(
            cmd,
            buffer,
            buf.len(),
            block_size,
            block_count,
            DataDirection::Read,
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
        let request = self.submit_fifo_data_request(
            cmd,
            buffer,
            buf.len(),
            block_size,
            block_count,
            DataDirection::Write,
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
        self.set_bus_width(width);
        Ok(())
    }

    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
        self.program_timing(timing::TimingTable::sd_for_speed(speed)?)
    }

    fn switch_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        self.set_signal_voltage(voltage)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        PhytiumMci::enable_completion_irq(self);
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        PhytiumMci::disable_completion_irq(self);
        Ok(())
    }

    fn handle_irq(&mut self) -> Self::Event {
        self.irq_handle().handle_irq()
    }
}

impl SdioIrqHost for PhytiumMci {
    type IrqHandle = PhytiumMciIrqHandle;

    fn irq_handle(&self) -> Self::IrqHandle {
        PhytiumMci::irq_handle(self)
    }

    fn completion_irq_enabled(&self) -> bool {
        PhytiumMci::completion_irq_enabled(self)
    }
}

#[cfg(test)]
mod tests {
    use sdmmc_protocol::{
        cmd::CMD0,
        response::ResponseType,
        sdio::{ClockSpeed, SignalVoltage},
    };

    use crate::{
        command::encode_command,
        regs::{Ctrl, Uhs},
        timing::{MediaKind, TimingTable},
    };

    #[test]
    fn sd_timing_table_matches_phytium_sd_values() {
        let init = TimingTable::for_speed(ClockSpeed::Identification, MediaKind::Sd).unwrap();
        assert_eq!(init.clk_div, 0x7e7dfa);
        assert_eq!(init.clk_src, 0x000502);
        assert!(init.use_hold);

        let hs = TimingTable::for_speed(ClockSpeed::HighSpeed, MediaKind::Sd).unwrap();
        assert_eq!(hs.clk_div, 0x030204);
        assert_eq!(hs.clk_src, 0x000502);
        assert!(hs.use_hold);
    }

    #[test]
    fn mmc_timing_table_uses_mmc_specific_rates() {
        let default = TimingTable::for_speed(ClockSpeed::Default, MediaKind::Mmc).unwrap();
        assert_eq!(default.target_hz, 26_000_000);

        let high = TimingTable::for_speed(ClockSpeed::HighSpeed, MediaKind::Mmc).unwrap();
        assert_eq!(high.target_hz, 52_000_000);
    }

    #[test]
    fn unsupported_sd_clock_modes_are_rejected() {
        assert!(TimingTable::sd_for_speed(ClockSpeed::Sdr104).is_err());
    }

    #[test]
    fn ctrl_register_bits_match_phytium_mci_layout() {
        let reg = Ctrl::new()
            .with_int_enable(true)
            .with_dma_enable(true)
            .with_read_wait(true)
            .with_use_internal_dmac(true);

        assert_eq!(reg.into_bits(), (1 << 4) | (1 << 5) | (1 << 6) | (1 << 25));
    }

    #[test]
    fn r3_command_encoding_does_not_enable_crc_check() {
        let cmd = sdmmc_protocol::cmd::Command {
            cmd: 1,
            arg: 0,
            resp_type: ResponseType::R3,
        };
        let reg = encode_command(&cmd, None);
        assert!(reg.response_expect());
        assert!(!reg.check_response_crc());
    }

    #[test]
    fn cmd0_encoding_sends_initialization_clocks() {
        let reg = encode_command(&CMD0, None);
        assert!(reg.send_initialization());
        assert!(!reg.response_expect());
    }

    #[test]
    fn cmd12_encoding_marks_stop_abort() {
        let reg = encode_command(&sdmmc_protocol::cmd::CMD12, None);
        assert!(reg.stop_abort_cmd());
    }

    #[test]
    fn uhs_voltage_bit_tracks_signal_voltage() {
        let v180 = crate::host::uhs_bits_after_voltage(Uhs::new(), SignalVoltage::V180).unwrap();
        assert_eq!(v180.volt(), 1);

        let v330 = crate::host::uhs_bits_after_voltage(v180, SignalVoltage::V330).unwrap();
        assert_eq!(v330.volt(), 0);
    }

    #[test]
    fn command_register_keeps_hold_register_optional() {
        let cmd = sdmmc_protocol::cmd::Command {
            cmd: 17,
            arg: 0,
            resp_type: ResponseType::R1,
        };
        let without_hold = encode_command(&cmd, None).with_use_hold_reg(false);
        assert!(!without_hold.use_hold_reg());
        assert_eq!(without_hold.cmd_index(), 17);
    }
}
