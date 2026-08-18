extern crate std;

use core::ptr::NonNull;

use sdio_host2::{BusWidth, ClockSpeed, ProgressCause, RequestProgress, SignalVoltage};
use tock_registers::interfaces::{Readable, Writeable};

use super::*;
use crate::platform::*;

const REG_CLOCK_CONTROL: usize = 0x2c;
const CLOCK_INTERNAL_ENABLE: u16 = 1;
const CLOCK_CONTROL_375MHZ_TO_25MHZ: u16 = (8 << 8) | CLOCK_INTERNAL_ENABLE;
const CLOCK_INTERNAL_STABLE: u16 = 1 << 1;
const REGISTER_RETRY_DELAY: core::time::Duration = core::time::Duration::from_micros(100);

#[repr(align(4))]
struct FakeMmio<const N: usize>([u8; N]);

impl<const N: usize> FakeMmio<N> {
    fn new() -> Self {
        Self([0; N])
    }

    fn base(&mut self) -> NonNull<u8> {
        NonNull::new(self.0.as_mut_ptr()).unwrap()
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

fn mark_clock_stable(core: &mut FakeMmio<0x400>) {
    let clock_control = unsafe { core.base().as_ptr().add(REG_CLOCK_CONTROL).cast::<u16>() };
    unsafe {
        clock_control.write_volatile(clock_control.read_volatile() | CLOCK_INTERNAL_STABLE);
    }
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
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetBusWidth(BusWidth::Bit4),
        )
    };

    assert!(matches!(result, Err(sdio_host2::Error::Unsupported)));
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

    let clock_result = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetClock(ClockSpeed::Sdr50),
        )
    };
    assert!(matches!(clock_result, Err(sdio_host2::Error::Unsupported)));

    let voltage_result = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V180),
        )
    };
    assert!(matches!(
        voltage_result,
        Err(sdio_host2::Error::Unsupported)
    ));
}

#[test]
fn high_speed_mode_sets_host_timing_even_when_clock_is_capped() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());

    let mut request = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetClock(ClockSpeed::HighSpeed),
        )
    }
    .unwrap();
    assert!(matches!(
        sdio_host2::SdioHost::advance_bus_op(&mut host, &mut request, ProgressCause::Submitted,)
            .unwrap(),
        RequestProgress::RegisterPending { .. }
    ));

    let mmio = Cv181xMmio::new(core.base(), syscon.base());
    let registers = mmio.core_registers();
    assert!(registers.host_control1.is_set(HOST_CONTROL1::HIGH_SPEED));
    assert_eq!(
        registers.host_control2.read(HOST_CONTROL2::UHS_MODE),
        HOST_CTRL2_UHS_SDR25
    );
    mark_clock_stable(&mut core);
    assert_eq!(
        sdio_host2::SdioHost::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        )
        .unwrap(),
        RequestProgress::Complete(Ok(()))
    );
}

#[test]
fn clock_transition_observes_stable_after_submission() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());
    let mut request = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetClock(ClockSpeed::Default),
        )
    }
    .unwrap();

    assert_eq!(
        sdio_host2::SdioHost::advance_bus_op(&mut host, &mut request, ProgressCause::Submitted,)
            .unwrap(),
        RequestProgress::RegisterPending {
            retry_after: REGISTER_RETRY_DELAY,
        },
        "CV181x clock setup must use the SDHCI Host2 register state machine",
    );
    let clock_control = unsafe {
        core.base()
            .as_ptr()
            .add(REG_CLOCK_CONTROL)
            .cast::<u16>()
            .read_volatile()
    };
    assert_eq!(
        clock_control, CLOCK_CONTROL_375MHZ_TO_25MHZ,
        "the generic divider must use the configured CV181x input clock",
    );

    mark_clock_stable(&mut core);

    assert_eq!(
        sdio_host2::SdioHost::advance_bus_op(
            &mut host,
            &mut request,
            ProgressCause::RegisterRetry,
        )
        .unwrap(),
        RequestProgress::Complete(Ok(())),
        "a stable clock observed after submission must complete the same request",
    );
}
