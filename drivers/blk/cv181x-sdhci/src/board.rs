//! CV181x TOP, pinmux, pad and PHY policy.

use tock_registers::interfaces::{ReadWriteable, Writeable};

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
        let registers = self.mmio.syscon_registers();
        let active_cd_func = if self.config.has_card_detect_gpio {
            PINMUX_FUNC_XGPIO
        } else {
            PINMUX_FUNC_SDIO0
        };
        registers.sdio0_cd_mux.set(active_cd_func);

        if self.config.touch_power_enable_pin {
            registers.sdio0_power_enable_mux.set(PINMUX_FUNC_SDIO0);
        }

        let func = if unplug {
            PINMUX_FUNC_XGPIO
        } else {
            PINMUX_FUNC_SDIO0
        };
        for register in [
            &registers.sdio0_clk_mux,
            &registers.sdio0_cmd_mux,
            &registers.sdio0_d0_mux,
            &registers.sdio0_d1_mux,
            &registers.sdio0_d2_mux,
            &registers.sdio0_d3_mux,
        ] {
            register.set(func);
        }
    }

    pub fn setup_sd_io(&mut self, reset: bool) {
        let registers = self.mmio.syscon_registers();
        set_pull(&registers.sdio0_cd_pull, PullMode::Up);
        set_pull(&registers.sdio0_power_enable_pull, PullMode::Down);
        set_pull(&registers.sdio0_clk_pull, PullMode::Down);

        let mode = if reset { PullMode::Down } else { PullMode::Up };
        for register in [
            &registers.sdio0_cmd_pull,
            &registers.sdio0_d0_pull,
            &registers.sdio0_d1_pull,
            &registers.sdio0_d2_pull,
            &registers.sdio0_d3_pull,
        ] {
            set_pull(register, mode);
        }
    }

    pub fn restore_ds_hs_phy(&mut self) {
        let registers = self.mmio.core_registers();
        registers.mshc_ctrl.modify(
            MSHC_CTRL::DS_HS_BIT_1::SET + MSHC_CTRL::DS_HS_BIT_8::SET + MSHC_CTRL::DS_HS_BIT_9::SET,
        );
        registers.phy_tx_rx_dly.set(PHY_TX_RX_DLY_DS_HS);
        registers.phy_config.set(PHY_CONFIG_DS_HS);
    }

    fn update_top_power(&mut self, low_bits: u32) {
        self.mmio
            .syscon_registers()
            .sd_powersw_ctrl
            .modify(TOP_SD_PWRSW_CTRL::LOW_BITS.val(low_bits));
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
