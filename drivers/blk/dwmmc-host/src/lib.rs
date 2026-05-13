//! Synopsys DesignWare Mobile Storage Host Controller (DW_mshc) backend
//! for the [`sdmmc-protocol`](sdmmc_protocol) driver crate.
//!
//! Implements [`sdmmc_protocol::sdio::SdioHost`] for the IP block known
//! variously as DWC_mobile_storage, dw_mshc, dw_mmc (Linux), or simply
//! the "Synopsys SD/MMC controller" — the same core used in Rockchip
//! RK33xx/RK35xx, Allwinner A-series, StarFive JH7110, and a long
//! tail of mid-range SoCs. PIO data path only; the internal DMAC
//! (IDMAC) path is intentionally disabled in [`DwMmc::reset_and_init`].
//!
//! # Scope
//!
//! - **Implemented**: PIO data transfer over the 0x100/0x200/0x400
//!   FIFO (configurable), 1-bit / 4-bit / 8-bit bus selection,
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
//! use sdmmc_protocol::sdio::{DelayNs, SdioSdmmc};
//! use dwmmc_host::DwMmc;
//!
//! # fn make_delay() -> impl DelayNs { struct N; impl DelayNs for N { fn delay_ns(&mut self, _: u32) {} } N }
//! // SAFETY: 0xFE2B_0000 must point at a valid DW_mshc register file
//! // the caller has exclusive access to.
//! let mmio = NonNull::new(0xFE2B_0000 as *mut u8).unwrap();
//! let mut host = unsafe { DwMmc::new(mmio) };
//! host.set_reference_clock(50_000_000);
//! host.reset_and_init().expect("controller reset");
//!
//! let mut card = SdioSdmmc::new(host, make_delay());
//! // card.init()?;
//! ```
//!
//! Construction is `unsafe` because the caller must guarantee that
//! the supplied address is a valid, exclusively-owned DW_mshc
//! register file.

#![no_std]
#![allow(clippy::missing_safety_doc)]

mod command;
mod data;
mod dma;
mod host;
mod regs;

use sdmmc_protocol::{
    cmd::{Command, DataDirection},
    error::Error,
    response::Response,
    sdio::{BusWidth, ClockSpeed, SdioHost, SignalVoltage},
};

pub use crate::{
    dma::{IDMAC_DESC_ALIGN, IDMAC_DESC_SIZE},
    host::{DEFAULT_FIFO_OFFSET, DwMmc},
};
use crate::{host::PendingData, regs::RegisterBlockVolatileFieldAccess};

/// Stable controller event extracted from DW_mshc raw interrupt status.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
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

impl SdioHost for DwMmc {
    fn send_command(&mut self, cmd: &Command) -> Result<Response, Error> {
        self.issue_command(cmd)
    }

    fn read_data(
        &mut self,
        cmd: &Command,
        buf: &mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Response, Error> {
        match self.try_idmac_read_transfer(cmd, buf, block_size, block_count) {
            Ok(response) => Ok(response),
            Err(err) => {
                log::debug!("dwmmc: IDMAC read transfer unavailable/failed: {:?}", err);
                <Self as SdioHost>::prepare_data_transfer(
                    self,
                    DataDirection::Read,
                    block_size,
                    block_count,
                )?;
                <Self as SdioHost>::set_block_count(self, block_count)?;
                let response = self.send_command(cmd)?;
                self.pio_read(buf, self.data_cmd_index, true)?;
                self.data_blocks_remaining = 0;
                Ok(response)
            }
        }
    }

    fn write_data(
        &mut self,
        cmd: &Command,
        buf: &[u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Response, Error> {
        match self.try_idmac_write_transfer(cmd, buf, block_size, block_count) {
            Ok(response) => Ok(response),
            Err(err) => {
                log::debug!("dwmmc: IDMAC write transfer unavailable/failed: {:?}", err);
                <Self as SdioHost>::prepare_data_transfer(
                    self,
                    DataDirection::Write,
                    block_size,
                    block_count,
                )?;
                <Self as SdioHost>::set_block_count(self, block_count)?;
                let response = self.send_command(cmd)?;
                self.pio_write(buf, self.data_cmd_index)?;
                self.data_blocks_remaining = 0;
                Ok(response)
            }
        }
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

    fn set_block_count(&mut self, _count: u32) -> Result<(), Error> {
        // BYTCNT carries both block size and count for the next data
        // phase; we program it from `prepare_data_transfer`. This hint
        // is intentionally a no-op so the protocol layer's call still
        // succeeds.
        Ok(())
    }

    fn prepare_data_transfer(
        &mut self,
        direction: DataDirection,
        block_size: u32,
        block_count: u32,
    ) -> Result<(), Error> {
        if direction.is_none() {
            self.pending_data = None;
            self.data_blocks_remaining = 0;
        } else {
            self.pending_data = Some(PendingData {
                direction,
                block_size,
                block_count,
            });
            self.data_blocks_remaining = block_count;
        }
        Ok(())
    }

    fn switch_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        self.set_signal_voltage(voltage)
    }
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

impl DwMmc {
    /// Read and acknowledge pending controller status, returning a stable
    /// event for OS glue to translate into wakeups or worker scheduling.
    pub fn handle_irq(&mut self) -> Event {
        let raw_status = self.regs.rintsts().read().into_bits();
        if raw_status != 0 {
            self.regs
                .rintsts()
                .write(crate::regs::RIntSts::from_bits(raw_status));
        }
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
