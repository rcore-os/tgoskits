extern crate std;

use core::ptr::NonNull;

use sdmmc_host::{BusWidth, ClockSpeed, SignalVoltage};
use tock_registers::interfaces::{Readable, Writeable};

use super::*;
use crate::{host2::AfterBusOp, platform::*};

#[repr(align(4))]
struct FakeMmio<const N: usize>([u8; N]);

impl<const N: usize> FakeMmio<N> {
    fn new() -> Self {
        Self([0; N])
    }

    fn base(&mut self) -> NonNull<u8> {
        NonNull::new(self.0.as_mut_ptr()).unwrap()
    }

    fn read_u32(&self, offset: usize) -> u32 {
        unsafe { core::ptr::read_unaligned(self.0.as_ptr().add(offset).cast()) }
    }

    fn write_u32(&mut self, offset: usize, value: u32) {
        unsafe { core::ptr::write_unaligned(self.0.as_mut_ptr().add(offset).cast(), value) };
    }
}

fn new_host<'a>(
    core: &'a mut FakeMmio<0x400>,
    syscon: &'a mut FakeMmio<0x2000>,
    config: Cv181xConfig,
) -> Cv181xSdhci {
    let mmio = Cv181xMmio::new(core.base(), syscon.base());
    unsafe { Cv181xSdhci::new(mmio, config) }
}

#[test]
fn sdio1_soc_setup_programs_pinmux_pull_clock_reset_and_card_detect() {
    let mut core = FakeMmio::<0x400>::new();
    let mut syscon = FakeMmio::<0x2000>::new();
    let mut crg = FakeMmio::<0x1000>::new();
    let mut rtcsys_ctrl = FakeMmio::<0x1000>::new();
    let mut rtcsys_io = FakeMmio::<0x1000>::new();
    for offset in [0x10d0, 0x10d4, 0x10d8, 0x10dc, 0x10e0, 0x10e4] {
        syscon.write_u32(offset, 0xffff_ffff);
    }
    crg.write_u32(0x30, u32::MAX);
    rtcsys_ctrl.write_u32(0x1c, u32::MAX);
    rtcsys_ctrl.write_u32(0x30, u32::MAX);

    let host = Cv181xMmio::new(core.base(), syscon.base());
    Cv181xSdio1Mmio::new(host, crg.base(), rtcsys_ctrl.base(), rtcsys_io.base()).initialize();

    for offset in [0x10d0, 0x10d4, 0x10d8, 0x10dc, 0x10e0, 0x10e4] {
        assert_eq!(syscon.read_u32(offset) & 0x7, 0);
    }
    for index in 0..20 {
        assert_eq!(rtcsys_io.read_u32(0x88 + index * 4), 0x1111_1111);
    }
    assert_eq!(
        crg.read_u32(0) & ((1 << 21) | (1 << 22) | (1 << 23)),
        (1 << 21) | (1 << 22) | (1 << 23)
    );
    assert_eq!(crg.read_u32(0x30) & (1 << 7), 0);
    assert_ne!(rtcsys_ctrl.read_u32(0x18) & (1 << 2), 0);
    assert_eq!(rtcsys_ctrl.read_u32(0x1c) & 0xf, 0);
    assert_ne!(syscon.read_u32(0x294) & ((1 << 8) | (1 << 9)), 0);
    assert_ne!(
        core.read_u32(0x200) & (1 << 16),
        0,
        "SDIO1 must select the controller's SD1 path before the first CMD52"
    );
}

#[test]
fn sdio1_reset_restores_its_own_soc_path_without_touching_sdio0_power() {
    let mut core = FakeMmio::<0x400>::new();
    let mut syscon = FakeMmio::<0x2000>::new();
    let mut crg = FakeMmio::<0x1000>::new();
    let mut rtcsys_ctrl = FakeMmio::<0x1000>::new();
    let mut rtcsys_io = FakeMmio::<0x1000>::new();
    let sdio0_power_sentinel = 0xa5a5_a5ae;
    syscon.write_u32(0x1f4, sdio0_power_sentinel);

    let host_mmio = Cv181xMmio::new(core.base(), syscon.base());
    let sdio1_mmio =
        Cv181xSdio1Mmio::new(host_mmio, crg.base(), rtcsys_ctrl.base(), rtcsys_io.base());
    let mut host = unsafe { Cv181xSdhci::new_sdio1(sdio1_mmio, Cv181xConfig::default()) };

    core.write_u32(0x200, 0);
    crg.write_u32(0, 0);
    rtcsys_ctrl.write_u32(0x18, 0);
    syscon.write_u32(0x294, 0);
    host.apply_after(AfterBusOp::ResetAll).unwrap();

    assert_eq!(syscon.read_u32(0x1f4), sdio0_power_sentinel);
    assert_ne!(core.read_u32(0x200) & (1 << 16), 0);
    assert_eq!(
        crg.read_u32(0) & ((1 << 21) | (1 << 22) | (1 << 23)),
        (1 << 21) | (1 << 22) | (1 << 23)
    );
    assert_ne!(rtcsys_ctrl.read_u32(0x18) & (1 << 2), 0);
    assert_ne!(syscon.read_u32(0x294) & ((1 << 8) | (1 << 9)), 0);
}

#[test]
fn power_on_sequence_configures_3v3_pads_io_and_ds_hs_phy() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mmio = Cv181xMmio::new(core.base(), syscon.base());
    mmio.syscon_registers().sd_powersw_ctrl.set(0xa5a5_a5a0);
    mmio.syscon_registers().sdio0_power_enable_mux.set(0x7);

    let mut host = new_host(
        &mut core,
        &mut syscon,
        Cv181xConfig {
            has_card_detect_gpio: true,
            ..Cv181xConfig::default()
        },
    );
    host.configure_sd_power_on();

    let registers = mmio.syscon_registers();
    assert_eq!(
        registers.sd_powersw_ctrl.get(),
        0xa5a5_a5a0 | TOP_SD_PWRSW_3V3
    );
    assert_eq!(registers.sdio0_cd_mux.get(), PINMUX_FUNC_XGPIO);
    assert_eq!(registers.sdio0_clk_mux.get(), PINMUX_FUNC_SDIO0);
    assert_eq!(registers.sdio0_cmd_mux.get(), PINMUX_FUNC_SDIO0);
    assert_eq!(registers.sdio0_d3_mux.get(), PINMUX_FUNC_SDIO0);
    assert_eq!(registers.sdio0_power_enable_mux.get(), 0x7);
    assert!(registers.sdio0_cmd_pull.is_set(PAD_PULL::UP));
    assert!(!registers.sdio0_cmd_pull.is_set(PAD_PULL::DOWN));

    let core_registers = mmio.core_registers();
    assert_eq!(core_registers.phy_tx_rx_dly.get(), PHY_TX_RX_DLY_DS_HS);
    assert_eq!(core_registers.phy_config.get(), PHY_CONFIG_DS_HS);
    assert!(core_registers.mshc_ctrl.is_set(MSHC_CTRL::DS_HS_BIT_1));
    assert!(core_registers.mshc_ctrl.is_set(MSHC_CTRL::DS_HS_BIT_8));
    assert!(core_registers.mshc_ctrl.is_set(MSHC_CTRL::DS_HS_BIT_9));
}

#[test]
fn power_off_switches_sd_pads_to_gpio_and_closes_power() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());

    host.configure_sd_power_off();

    let mmio = Cv181xMmio::new(core.base(), syscon.base());
    let registers = mmio.syscon_registers();
    assert_eq!(registers.sdio0_clk_mux.get(), PINMUX_FUNC_XGPIO);
    assert_eq!(registers.sdio0_d0_mux.get(), PINMUX_FUNC_XGPIO);
    assert!(registers.sdio0_d0_pull.is_set(PAD_PULL::DOWN));
    assert_eq!(
        registers.sd_powersw_ctrl.read(TOP_SD_PWRSW_CTRL::LOW_BITS),
        TOP_SD_PWRSW_OFF
    );
}

#[test]
fn config_normalization_keeps_clock_bounds_valid() {
    let config = Cv181xConfig {
        src_frequency_hz: 0,
        min_frequency_hz: 50_000_000,
        max_frequency_hz: 25_000_000,
        ..Cv181xConfig::default()
    }
    .normalized();

    assert_eq!(config.src_frequency_hz, DEFAULT_SRC_FREQUENCY_HZ);
    assert_eq!(config.max_frequency_hz, 50_000_000);
}

#[test]
fn bus_width_limit_rejects_width_above_board_wiring() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(
        &mut core,
        &mut syscon,
        Cv181xConfig {
            max_bus_width: BusWidth::Bit1,
            ..Cv181xConfig::default()
        },
    );

    let result = unsafe {
        sdmmc_host::SdMmcHost::submit_bus_op(
            &mut host,
            sdmmc_host::BusOp::SetBusWidth(BusWidth::Bit4),
        )
    };

    assert!(matches!(result, Err(sdmmc_host::Error::Unsupported)));
}

#[test]
fn clock_bus_request_cannot_bypass_an_active_transaction() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());
    let transaction = sdmmc_host::Transaction::command(sdmmc_protocol::cmd::CMD0);
    let _active =
        unsafe { sdmmc_host::SdMmcHost::submit_transaction(&mut host, transaction) }.unwrap();

    assert!(matches!(
        unsafe {
            sdmmc_host::SdMmcHost::submit_bus_op(
                &mut host,
                sdmmc_host::BusOp::SetClock(ClockSpeed::Identification),
            )
        },
        Err(sdmmc_host::Error::Busy)
    ));
}

#[test]
fn no_1v8_rejects_uhs_clock_and_voltage_paths() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(
        &mut core,
        &mut syscon,
        Cv181xConfig {
            no_1v8: true,
            ..Cv181xConfig::default()
        },
    );

    assert_eq!(
        host.set_clock_speed(ClockSpeed::Sdr50),
        Err(sdmmc_host::Error::Unsupported)
    );

    let result = unsafe {
        sdmmc_host::SdMmcHost::submit_bus_op(
            &mut host,
            sdmmc_host::BusOp::SetSignalVoltage(SignalVoltage::V180),
        )
    };

    assert!(matches!(result, Err(sdmmc_host::Error::Unsupported)));
}

#[test]
fn high_speed_mode_sets_host_timing_even_when_clock_is_capped() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());

    let _ = host.set_clock_speed(ClockSpeed::HighSpeed);

    let mmio = Cv181xMmio::new(core.base(), syscon.base());
    let registers = mmio.core_registers();
    assert!(registers.host_control1.is_set(HOST_CONTROL1::HIGH_SPEED));
    assert!(
        registers
            .host_control2
            .matches_all(HOST_CONTROL2::UHS_MODE::SDR25)
    );
}
