//! CV181x SDHCI clock and timing policy.

use sdmmc_host::ClockSpeed;
use tock_registers::{fields::FieldValue, interfaces::ReadWriteable};

use super::{Cv181xSdhci, map_protocol_error};
use crate::platform::*;

impl Cv181xSdhci {
    pub(super) fn program_clock(
        &mut self,
        target_hz: u32,
        high_speed: bool,
        uhs_mode: FieldValue<u16, HOST_CONTROL2::Register>,
    ) -> Result<(), sdmmc_host::Error> {
        let target_hz = self.config.clamp_clock(target_hz);
        self.set_host_timing_bits(high_speed, uhs_mode);
        self.inner
            .enable_clock(self.config.src_frequency_hz, target_hz)
            .map_err(map_protocol_error)
    }

    pub(super) fn set_clock_speed(&mut self, speed: ClockSpeed) -> Result<(), sdmmc_host::Error> {
        match speed {
            ClockSpeed::Identification => self.program_clock(
                self.config.min_frequency_hz,
                false,
                HOST_CONTROL2::UHS_MODE::SDR12,
            ),
            ClockSpeed::Default | ClockSpeed::Sdr12 => {
                self.program_clock(25_000_000, false, HOST_CONTROL2::UHS_MODE::SDR12)
            }
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => {
                // SG2002 SDIO1 becomes unreliable during large CMD53 firmware
                // writes above 25 MHz even though the card advertises SHS.
                self.program_clock(25_000_000, true, HOST_CONTROL2::UHS_MODE::SDR25)
            }
            ClockSpeed::Sdr50 | ClockSpeed::Sdr104 | ClockSpeed::Ddr50 | ClockSpeed::Hs200 => {
                Err(sdmmc_host::Error::Unsupported)
            }
            _ => Err(sdmmc_host::Error::Unsupported),
        }
    }

    fn set_host_timing_bits(
        &mut self,
        high_speed: bool,
        uhs_mode: FieldValue<u16, HOST_CONTROL2::Register>,
    ) {
        let registers = self.mmio.core_registers();
        if high_speed {
            registers
                .host_control1
                .modify(HOST_CONTROL1::HIGH_SPEED::SET);
        } else {
            registers
                .host_control1
                .modify(HOST_CONTROL1::HIGH_SPEED::CLEAR);
        }

        registers.host_control2.modify(uhs_mode);
    }
}
