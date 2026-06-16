//! SDHCI host controller backend for the `sdmmc-protocol` driver crate.
//!
//! This crate ports the [SD Host Controller Standard Specification][sdhci]
//! v3.x register layout and PIO data path into a [`SdioHost`] implementation
//! that the [`sdmmc_protocol::sdio::SdioSdmmc`] driver can drive directly.
//!
//! # Scope
//!
//! - **Implemented**: PIO transfers, **ADMA2 (32-bit) transfers**, 1-bit /
//!   4-bit bus, default-speed and high-speed clocking, 32-bit response
//!   slots, 136-bit R2 reconstruction, software reset / clock setup.
//! - **Out of scope (for now)**: 64-bit ADMA2, 8-bit eMMC bus, HS200 /
//!   SDR50 / SDR104 clocking, tuning (CMD19 / CMD21), eMMC-specific
//!   commands. 1.8 V signaling is wired up at the register level but is
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
//! let mut card = SdioSdmmc::new(host);
//! let mut scratch = SdioInitScratch::new();
//! let mut request = card.submit_init(&mut scratch)?;
//! // Poll request here. Runtime code chooses spin, yield, IRQ wait, or timer.
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! For block request I/O, use [`Sdhci::submit_read_blocks`] or
//! [`Sdhci::submit_write_blocks`] and complete the returned request with
//! [`Sdhci::poll_block_request`]. `BlockTransferMode::Dma` maps the request
//! buffer and builds the ADMA2 descriptor table; `BlockTransferMode::Fifo`
//! uses the controller FIFO with the same submit/poll contract:
//!
//! ```ignore
//! use core::{num::NonZeroUsize, ptr::NonNull};
//! use dma_api::DeviceDma;
//! use sdhci_host::{BlockRequestSlot, BlockTransferMode, RequestId, Sdhci};
//!
//! # use platform::DmaImpl;
//! let dma = DeviceDma::new(u32::MAX as u64, &DmaImpl);
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

use core::{marker::PhantomData, num::NonZeroUsize, ptr::NonNull};

mod command;
mod dma;
mod host;
mod regs;

pub use dma::{ADMA2_DESC_ALIGN, ADMA2_DESC_COUNT, BlockRequest, BlockRequestSlot, RequestId};
pub use host::{HostClock, Sdhci};
pub use sdmmc_protocol::block::{
    BlockBufferConfig, BlockPoll, BlockRequestId, BlockTransferDirection, BlockTransferMode,
    BlockTransferState,
};
use sdmmc_protocol::{
    DataCommandPoll,
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    sdio::{
        BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, SdioHost, SignalVoltage,
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

impl SdioHost for Sdhci {
    type Event = Event;
    type DataRequest<'a> = DataRequest<'a>;

    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
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
        let mut ctrl = self.read_u8(REG_HOST_CONTROL1);
        ctrl &= !(HOST_CTRL1_4BIT | HOST_CTRL1_8BIT);
        match width {
            BusWidth::Bit1 => {}
            BusWidth::Bit4 => ctrl |= HOST_CTRL1_4BIT,
            // 8-bit is eMMC territory and is intentionally not part of the
            // MVP — surface it as Unsupported so the protocol layer can
            // refuse cleanly instead of silently writing the bit and
            // misconfiguring the bus.
            BusWidth::Bit8 => return Err(Error::UnsupportedCommand),
            // Future BusWidth variants are not supported by this controller.
            _ => return Err(Error::UnsupportedCommand),
        }
        self.write_u8(REG_HOST_CONTROL1, ctrl);
        Ok(())
    }

    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
        let target_hz = match speed {
            ClockSpeed::Identification => 400_000,
            ClockSpeed::Default | ClockSpeed::Sdr12 => 25_000_000,
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => 50_000_000,
            ClockSpeed::Sdr50 | ClockSpeed::Ddr50 => 50_000_000,
            ClockSpeed::Sdr104 => 104_000_000,
            ClockSpeed::Hs200 => 200_000_000,
            // Future ClockSpeed variants are not supported by this controller.
            _ => return Err(Error::UnsupportedCommand),
        };

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
            SignalVoltage::V120 => return Err(Error::UnsupportedCommand),
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

    fn execute_tuning(&mut self, cmd_index: u8) -> Result<(), Error> {
        // Only CMD19 (SD UHS-I) and CMD21 (eMMC HS200) make sense here.
        // Reject anything else loudly so the protocol layer doesn't
        // accidentally tune for a non-tuning command.
        if cmd_index != 19 && cmd_index != 21 {
            return Err(Error::InvalidArgument);
        }

        // Block size for the tuning data phase: SD CMD19 always 64,
        // MMC CMD21 is 64 (4-bit) or 128 (8-bit). The host doesn't
        // know the bus width here without snooping HOST_CONTROL1; we
        // read it back to pick the right size.
        let block_size: u16 =
            if cmd_index == 21 && self.read_u8(REG_HOST_CONTROL1) & HOST_CTRL1_8BIT != 0 {
                128
            } else {
                64
            };

        // Pre-program the data registers per SDHCI v3 §3.7.7. The
        // controller issues the tuning command itself; we just hand it
        // the shape of the data phase.
        self.write_u16(REG_BLOCK_SIZE, block_size & 0x0FFF);
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
        Sdhci::handle_irq(self)
    }
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

pub(crate) fn event_from_status(normal: u16, error: u16) -> Event {
    if normal & NORMAL_INT_ERROR != 0 {
        Event::Error { normal, error }
    } else if normal & NORMAL_INT_CMD_COMPLETE != 0 {
        Event::CommandComplete
    } else if normal & NORMAL_INT_XFER_COMPLETE != 0 {
        Event::TransferComplete
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
            Event::Error { .. } => HostEventKind::Error,
            Event::Other { .. } => HostEventKind::Other,
        }
    }

    fn source(&self) -> HostEventSource {
        match self {
            Event::CommandComplete => HostEventSource::Command,
            Event::TransferComplete => HostEventSource::Data,
            Event::None | Event::Error { .. } | Event::Other { .. } => HostEventSource::Controller,
        }
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        match self {
            Event::TransferComplete => Some(BlockRequestId::new(0)),
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

    /// Read and acknowledge pending controller status, returning a stable
    /// event for OS glue to translate into wakeups or worker scheduling.
    pub fn handle_irq(&mut self) -> Event {
        let normal = self.read_u16(REG_NORMAL_INT_STATUS);
        let error = if normal & NORMAL_INT_ERROR != 0 {
            self.read_u16(REG_ERROR_INT_STATUS)
        } else {
            0
        };

        if normal != 0 {
            self.write_u16(REG_NORMAL_INT_STATUS, normal);
        }
        if error != 0 {
            self.write_u16(REG_ERROR_INT_STATUS, error);
        }
        self.irq_pending_normal |= normal;
        self.irq_pending_error |= error;

        event_from_status(normal, error)
    }
}

#[cfg(test)]
mod tests {
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
    fn exposes_block_buffer_constraints() {
        let host = unsafe { Sdhci::new_from_addr(0x1000_0000) };

        let dma = host.block_buffer_config(BlockTransferMode::Dma);
        assert_eq!(dma.block_size.get(), 512);
        assert_eq!(dma.align, 512);
        assert_eq!(dma.dma_mask, Some(u32::MAX as u64));
    }
}
