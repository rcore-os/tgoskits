//! CV181x SDHCI clock and timing policy.

use sdmmc_host::ClockSpeed;
use tock_registers::{fields::FieldValue, interfaces::ReadWriteable};

use super::Cv181xSdhci;
use crate::platform::*;

#[derive(Clone, Copy)]
pub(super) struct Cv181xClockPlan {
    pub target_hz: u32,
    high_speed: bool,
    uhs_mode: FieldValue<u16, HOST_CONTROL2::Register>,
}

impl Cv181xSdhci {
    pub(super) fn clock_hz_plan(&self, target_hz: u32) -> Cv181xClockPlan {
        Cv181xClockPlan {
            target_hz: self.config.clamp_clock(target_hz),
            high_speed: target_hz > DEFAULT_MAX_FREQUENCY_HZ,
            uhs_mode: HOST_CONTROL2::UHS_MODE::SDR12,
        }
    }

    pub(super) fn clock_plan(
        &self,
        speed: ClockSpeed,
    ) -> Result<Cv181xClockPlan, sdmmc_host::Error> {
        let (target_hz, high_speed, uhs_mode) = match speed {
            ClockSpeed::Identification => (
                self.config.min_frequency_hz,
                false,
                HOST_CONTROL2::UHS_MODE::SDR12,
            ),
            ClockSpeed::Default | ClockSpeed::Sdr12 => {
                (25_000_000, false, HOST_CONTROL2::UHS_MODE::SDR12)
            }
            // SG2002 SDIO1 becomes unreliable during large CMD53 firmware
            // writes above 25 MHz even though the card advertises SHS.
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => {
                (25_000_000, true, HOST_CONTROL2::UHS_MODE::SDR25)
            }
            ClockSpeed::Sdr50 | ClockSpeed::Sdr104 | ClockSpeed::Ddr50 | ClockSpeed::Hs200 => {
                return Err(sdmmc_host::Error::Unsupported);
            }
            _ => return Err(sdmmc_host::Error::Unsupported),
        };
        Ok(Cv181xClockPlan {
            target_hz: self.config.clamp_clock(target_hz),
            high_speed,
            uhs_mode,
        })
    }

    pub(super) fn apply_clock_timing(&mut self, plan: Cv181xClockPlan) {
        let registers = self.mmio.core_registers();
        if plan.high_speed {
            registers
                .host_control1
                .modify(HOST_CONTROL1::HIGH_SPEED::SET);
        } else {
            registers
                .host_control1
                .modify(HOST_CONTROL1::HIGH_SPEED::CLEAR);
        }

        registers.host_control2.modify(plan.uhs_mode);
    }
}
