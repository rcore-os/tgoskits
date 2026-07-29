//! CV181x TOP, pinmux, pad and PHY policy.

use super::{Cv181xSdhci, host2::AfterBusOp};
use crate::platform::*;

impl Cv181xSdhci {
    pub fn configure_sd_power_on(&mut self) {
        self.restore_3v3_power();
        self.setup_sd_pad(false);
        self.setup_sd_io(false);
        self.restore_ds_hs_phy();
    }

    pub fn configure_sd_power_off(&mut self) {
        self.setup_sd_pad(true);
        self.setup_sd_io(true);
        self.close_power();
    }

    pub fn restore_3v3_power(&mut self) {
        self.update_top_power(TOP_SD_PWRSW_3V3);
    }

    pub fn close_power(&mut self) {
        self.update_top_power(TOP_SD_PWRSW_OFF);
    }

    pub fn setup_sd_pad(&mut self, unplug: bool) {
        let pinmux = self.mmio.pinmux();
        let active_cd_func = if self.config.has_card_detect_gpio {
            PINMUX_FUNC_XGPIO
        } else {
            PINMUX_FUNC_SDIO0
        };
        write_u8(pinmux, PINMUX_SDIO0_CD, active_cd_func);

        if self.config.touch_power_enable_pin {
            write_u8(pinmux, PINMUX_SDIO0_PWR_EN, PINMUX_FUNC_SDIO0);
        }

        let func = if unplug {
            PINMUX_FUNC_XGPIO
        } else {
            PINMUX_FUNC_SDIO0
        };
        for off in [
            PINMUX_SDIO0_CLK,
            PINMUX_SDIO0_CMD,
            PINMUX_SDIO0_D0,
            PINMUX_SDIO0_D1,
            PINMUX_SDIO0_D2,
            PINMUX_SDIO0_D3,
        ] {
            write_u8(pinmux, off, func);
        }
    }

    pub fn setup_sd_io(&mut self, reset: bool) {
        let pinmux = self.mmio.pinmux();
        set_pull(pinmux, IO_SDIO0_CD, IO_PULL_UP, IO_PULL_DOWN);
        set_pull(pinmux, IO_SDIO0_PWR_EN, IO_PULL_DOWN, IO_PULL_UP);
        set_pull(pinmux, IO_SDIO0_CLK, IO_PULL_DOWN, IO_PULL_UP);

        let (set, clear) = if reset {
            (IO_PULL_DOWN, IO_PULL_UP)
        } else {
            (IO_PULL_UP, IO_PULL_DOWN)
        };
        for off in [
            IO_SDIO0_CMD,
            IO_SDIO0_D0,
            IO_SDIO0_D1,
            IO_SDIO0_D2,
            IO_SDIO0_D3,
        ] {
            set_pull(pinmux, off, set, clear);
        }
    }

    pub fn restore_ds_hs_phy(&mut self) {
        let core = self.mmio.core();
        let mshc = read_u32(core, CVI_VENDOR_MSHC_CTRL) | MSHC_CTRL_DS_HS_BITS;
        write_u32(core, CVI_VENDOR_MSHC_CTRL, mshc);
        write_u32(core, CVI_PHY_TX_RX_DLY, PHY_TX_RX_DLY_DS_HS);
        write_u32(core, CVI_PHY_CONFIG, PHY_CONFIG_DS_HS);
    }

    fn update_top_power(&mut self, low_bits: u32) {
        let cur = read_u32(self.mmio.syscon(), TOP_SD_PWRSW_CTRL);
        write_u32(
            self.mmio.syscon(),
            TOP_SD_PWRSW_CTRL,
            (cur & !TOP_SD_PWRSW_LOW_MASK) | low_bits,
        );
    }

    pub(super) fn apply_after(&mut self, after: AfterBusOp) -> Result<(), sdio_host2::Error> {
        match after {
            AfterBusOp::None => Ok(()),
            AfterBusOp::PowerOn | AfterBusOp::ResetAll => {
                self.configure_sd_power_on();
                Ok(())
            }
        }
    }
}
