//! CV181x SDHCI clock and timing policy.

use sdio_host2::ClockSpeed;

use super::{Cv181xSdhci, map_protocol_error};
use crate::platform::*;

impl Cv181xSdhci {
    pub(super) fn program_clock(
        &mut self,
        target_hz: u32,
        high_speed: bool,
        uhs_mode: u16,
    ) -> Result<(), sdio_host2::Error> {
        let target_hz = self.config.clamp_clock(target_hz);
        self.set_host_timing_bits(high_speed, uhs_mode);
        self.inner
            .enable_clock(self.config.src_frequency_hz, target_hz)
            .map_err(map_protocol_error)
    }

    pub(super) fn set_clock_speed(&mut self, speed: ClockSpeed) -> Result<(), sdio_host2::Error> {
        match speed {
            ClockSpeed::Identification => {
                self.program_clock(self.config.min_frequency_hz, false, HOST_CTRL2_UHS_SDR12)
            }
            ClockSpeed::Default | ClockSpeed::Sdr12 => {
                self.program_clock(25_000_000, false, HOST_CTRL2_UHS_SDR12)
            }
            ClockSpeed::HighSpeed | ClockSpeed::Sdr25 => {
                self.program_clock(50_000_000, true, HOST_CTRL2_UHS_SDR25)
            }
            ClockSpeed::Sdr50 | ClockSpeed::Sdr104 | ClockSpeed::Ddr50 | ClockSpeed::Hs200 => {
                Err(sdio_host2::Error::Unsupported)
            }
            _ => Err(sdio_host2::Error::Unsupported),
        }
    }

    fn set_host_timing_bits(&mut self, high_speed: bool, uhs_mode: u16) {
        let core = self.mmio.core();
        let mut ctrl1 = read_u8(core, REG_HOST_CONTROL1);
        if high_speed {
            ctrl1 |= HOST_CTRL1_HIGH_SPEED;
        } else {
            ctrl1 &= !HOST_CTRL1_HIGH_SPEED;
        }
        write_u8(core, REG_HOST_CONTROL1, ctrl1);

        let ctrl2 = (read_u16(core, REG_HOST_CONTROL2) & !HOST_CTRL2_UHS_MODE_MASK)
            | (uhs_mode & HOST_CTRL2_UHS_MODE_MASK);
        write_u16(core, REG_HOST_CONTROL2, ctrl2);
    }
}
