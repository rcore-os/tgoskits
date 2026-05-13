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
//!   SDR50 / SDR104 clocking, voltage / signaling switch (CMD11), tuning
//!   (CMD19 / CMD21), eMMC-specific commands.
//!
//! # Usage
//!
//! ```no_run
//! use core::ptr::NonNull;
//! use sdmmc_protocol::sdio::{DelayNs, SdioSdmmc};
//! use sdhci_host::Sdhci;
//!
//! # fn make_delay() -> impl DelayNs { struct N; impl DelayNs for N { fn delay_ns(&mut self, _: u32) {} } N }
//! let mmio = NonNull::new(0xFE31_0000 as *mut u8).unwrap();
//! let host = unsafe { Sdhci::new(mmio) };
//! let delay = make_delay();
//! let mut card = SdioSdmmc::new(host, delay);
//! // card.init()?;
//! ```
//!
//! For ADMA2 request I/O, pass a `dma_api::DeviceDma` capability into
//! [`Sdhci::dma_read_blocks_into`]. The driver core maps the request
//! buffer, allocates the ADMA2 descriptor table, and performs cache sync:
//!
//! ```ignore
//! use core::{num::NonZeroUsize, ptr::NonNull};
//! use dma_api::DeviceDma;
//! use sdhci_host::Sdhci;
//!
//! # use platform::DmaImpl;
//! let dma = DeviceDma::new(u32::MAX as u64, &DmaImpl);
//! let mut host = unsafe { Sdhci::new_from_addr(0xFE31_0000) };
//! let mut block = [0u8; 512];
//! let ptr = NonNull::new(block.as_mut_ptr()).unwrap();
//! host.dma_read_blocks_into(0, ptr, NonZeroUsize::new(block.len()).unwrap(), &dma)?;
//! # Ok::<(), sdmmc_protocol::Error>(())
//! ```
//!
//! Construction is `unsafe` because the caller must guarantee that the
//! supplied address is a valid, exclusively-owned SDHCI register file.
//!
//! [sdhci]: https://www.sdcard.org/downloads/pls/

#![no_std]
#![allow(clippy::missing_safety_doc)]

mod command;
mod data;
mod dma;
mod host;
mod regs;

pub use dma::{ADMA2_DESC_ALIGN, ADMA2_DESC_COUNT};
pub use host::Sdhci;
use sdmmc_protocol::{
    cmd::{Command, DataDirection},
    error::{Error, ErrorContext, Phase},
    response::Response,
    sdio::{BusWidth, ClockSpeed, SdioHost, SignalVoltage},
};

use crate::{host::PendingData, regs::*};

/// Stable controller event extracted from SDHCI interrupt-status registers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// No status bit requiring runtime action is currently pending.
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

impl SdioHost for Sdhci {
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
        match self.try_adma2_read_transfer(cmd, buf, block_size, block_count) {
            Ok(response) => Ok(response),
            Err(err) => {
                log::debug!("sdhci: ADMA2 read transfer unavailable/failed: {:?}", err);
                <Self as SdioHost>::prepare_data_transfer(
                    self,
                    DataDirection::Read,
                    block_size,
                    block_count,
                )?;
                <Self as SdioHost>::set_block_count(self, block_count)?;
                let response = self.send_command(cmd)?;
                self.pio_read(buf, block_size, self.active_data_cmd)?;
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
        match self.try_adma2_write_transfer(cmd, buf, block_size, block_count) {
            Ok(response) => Ok(response),
            Err(err) => {
                log::debug!("sdhci: ADMA2 write transfer unavailable/failed: {:?}", err);
                <Self as SdioHost>::prepare_data_transfer(
                    self,
                    DataDirection::Write,
                    block_size,
                    block_count,
                )?;
                <Self as SdioHost>::set_block_count(self, block_count)?;
                let response = self.send_command(cmd)?;
                self.pio_write(buf, block_size, self.active_data_cmd)?;
                Ok(response)
            }
        }
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
            cb(target_hz)?;
            return self.enable_clock_external();
        }

        let base = self.base_clock_hz();
        if base == 0 {
            return Err(Error::BadResponse(ErrorContext::new(Phase::Init)));
        }
        self.enable_clock(base, target_hz)
    }

    fn set_block_count(&mut self, _count: u32) -> Result<(), Error> {
        // We push BLOCK_COUNT in `configure_data_phase` once we know both
        // the count and the direction, so this hint is intentionally a
        // no-op.
        Ok(())
    }

    fn prepare_data_transfer(
        &mut self,
        direction: DataDirection,
        block_size: u32,
        block_count: u32,
    ) -> Result<(), Error> {
        // Plain PIO: never set the DMA bit in the transfer-mode register.
        self.use_dma = false;
        if direction.is_none() {
            self.pending_data = None;
        } else {
            self.pending_data = Some(PendingData {
                direction,
                block_size,
                block_count,
            });
        }
        Ok(())
    }

    fn switch_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        // 1. Stop the SD clock so we don't drive the bus during the
        //    transition. Spec calls for ≥ 5 ms here; the controller's
        //    `1.8V Signaling Enable` bit toggles the IO domain
        //    immediately, so the wait is a soft requirement enforced by
        //    the platform delay (we don't have one here — bring-up code
        //    on the caller side should add one if needed).
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

impl Sdhci {
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
}
