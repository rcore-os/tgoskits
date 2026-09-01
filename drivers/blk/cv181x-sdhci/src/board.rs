//! CV181x TOP, pinmux, pad and PHY policy.

use tock_registers::interfaces::{ReadWriteable, Writeable};

use super::{ControllerResources, Cv181xSdhci, host2::AfterBusOp};
use crate::platform::*;

impl Cv181xSdhci {
    pub fn configure_sd_power_on(&mut self) {
        configure_sd_power_on(self.mmio, self.config);
    }

    pub fn configure_sd_power_off(&mut self) {
        configure_sd_power_off(self.mmio, self.config);
    }

    pub fn restore_3v3_power(&mut self) {
        update_top_power(self.mmio, TOP_SD_PWRSW_3V3);
    }

    pub fn close_power(&mut self) {
        update_top_power(self.mmio, TOP_SD_PWRSW_OFF);
    }

    pub fn setup_sd_pad(&mut self, unplug: bool) {
        setup_sd_pad(self.mmio, self.config, unplug);
    }

    pub fn setup_sd_io(&mut self, reset: bool) {
        setup_sd_io(self.mmio, reset);
    }

    pub fn restore_ds_hs_phy(&mut self) {
        restore_ds_hs_phy(self.mmio);
    }

    pub(super) fn restore_controller_after_reset(&mut self) {
        restore_controller_after_reset(self.mmio, self.config, self.controller);
    }

    fn configure_controller_power_off(&mut self) {
        if matches!(self.controller, ControllerResources::Sd) {
            self.configure_sd_power_off();
        }
    }

    fn restore_controller_3v3(&mut self) {
        if matches!(self.controller, ControllerResources::Sd) {
            self.restore_3v3_power();
        }
    }

    pub(super) fn apply_after(&mut self, after: AfterBusOp) -> Result<(), sdmmc_host::Error> {
        match after {
            AfterBusOp::None => Ok(()),
            AfterBusOp::PowerOn | AfterBusOp::ResetAll => {
                self.restore_controller_after_reset();
                Ok(())
            }
            AfterBusOp::PowerOff => {
                self.configure_controller_power_off();
                Ok(())
            }
            AfterBusOp::Restore3v3 => {
                self.restore_controller_3v3();
                Ok(())
            }
        }
    }
}

pub(super) fn restore_controller_after_reset(
    mmio: Cv181xMmio,
    config: Cv181xConfig,
    controller: ControllerResources,
) {
    match controller {
        ControllerResources::Sd => configure_sd_power_on(mmio, config),
        ControllerResources::Sdio1(sdio1) => {
            sdio1.initialize();
            restore_ds_hs_phy(mmio);
        }
    }
}

fn configure_sd_power_on(mmio: Cv181xMmio, config: Cv181xConfig) {
    update_top_power(mmio, TOP_SD_PWRSW_3V3);
    setup_sd_pad(mmio, config, false);
    setup_sd_io(mmio, false);
    restore_ds_hs_phy(mmio);
}

fn configure_sd_power_off(mmio: Cv181xMmio, config: Cv181xConfig) {
    setup_sd_pad(mmio, config, true);
    setup_sd_io(mmio, true);
    update_top_power(mmio, TOP_SD_PWRSW_OFF);
}

fn setup_sd_pad(mmio: Cv181xMmio, config: Cv181xConfig, unplug: bool) {
    let registers = mmio.syscon_registers();
    let active_cd_func = if config.has_card_detect_gpio {
        PINMUX_FUNC_XGPIO
    } else {
        PINMUX_FUNC_SDIO0
    };
    registers.sdio0_cd_mux.set(active_cd_func);

    if config.touch_power_enable_pin {
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

fn setup_sd_io(mmio: Cv181xMmio, reset: bool) {
    let registers = mmio.syscon_registers();
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

fn update_top_power(mmio: Cv181xMmio, low_bits: u32) {
    mmio.syscon_registers()
        .sd_powersw_ctrl
        .modify(TOP_SD_PWRSW_CTRL::LOW_BITS.val(low_bits));
}

pub(super) fn restore_ds_hs_phy(mmio: Cv181xMmio) {
    let registers = mmio.core_registers();
    registers.mshc_ctrl.modify(
        MSHC_CTRL::DS_HS_BIT_1::SET + MSHC_CTRL::DS_HS_BIT_8::SET + MSHC_CTRL::DS_HS_BIT_9::SET,
    );
    registers.phy_tx_rx_dly.set(PHY_TX_RX_DLY_DS_HS);
    registers.phy_config.set(PHY_CONFIG_DS_HS);
}
