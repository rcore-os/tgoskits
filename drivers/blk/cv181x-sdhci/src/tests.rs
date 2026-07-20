extern crate std;

use super::*;

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

fn new_host(
    core: &mut FakeMmio<0x400>,
    syscon: &mut FakeMmio<0x2000>,
    config: Cv181xConfig,
) -> Cv181xSdhci {
    let mmio = unsafe { Cv181xMmio::new(core.base(), syscon.base()) };
    Cv181xSdhci::new(mmio, config)
}

fn poll_ready_bus_op(
    host: &mut Cv181xSdhci,
    request: &mut BusRequest,
) -> Result<(), sdio_host2::Error> {
    match sdio_host2::SdioHost::poll_bus_op(host, request).unwrap() {
        RequestPoll::Ready(result) => result,
        RequestPoll::Pending => panic!("test bus op should complete synchronously"),
    }
}

#[test]
fn discovery_constructor_does_not_program_controller_or_board_registers() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let core_before = core.0;
    let syscon_before = syscon.0;

    let host = new_host(&mut core, &mut syscon, Cv181xConfig::default());
    drop(host);

    assert!(core.0 == core_before, "discovery modified controller MMIO");
    assert!(syscon.0 == syscon_before, "discovery modified board MMIO");
}

#[test]
fn rejected_power_off_does_not_modify_board_registers() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());
    let _active = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetBusWidth(BusWidth::Bit1),
        )
    }
    .unwrap();
    let syscon_before = syscon.0;

    let result =
        unsafe { sdio_host2::SdioHost::submit_bus_op(&mut host, sdio_host2::BusOp::PowerOff) };

    assert!(matches!(result, Err(sdio_host2::Error::Busy)));
    assert!(
        syscon.0 == syscon_before,
        "rejected PowerOff modified board MMIO"
    );
}

#[test]
fn rejected_3v3_transition_does_not_modify_board_registers() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());
    let _active = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetBusWidth(BusWidth::Bit1),
        )
    }
    .unwrap();
    let syscon_before = syscon.0;

    let result = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V330),
        )
    };

    assert!(matches!(result, Err(sdio_host2::Error::Busy)));
    assert!(
        syscon.0 == syscon_before,
        "rejected voltage change modified board MMIO"
    );
}

#[test]
fn completion_delivery_requires_one_shot_irq_source_transfer() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());

    assert_eq!(
        host.enable_completion_irq(),
        Err(ProtocolError::InvalidArgument)
    );
    let _source = host.take_irq_source().expect("first transfer must succeed");
    assert!(host.take_irq_source().is_none());
    host.enable_completion_irq().unwrap();
}

#[test]
fn combined_transfer_and_error_irq_is_classified_error_first() {
    const NORMAL_INT_STATUS: usize = 0x30;
    const ERROR_INT_STATUS: usize = 0x32;
    const TRANSFER_COMPLETE: u16 = 1 << 1;
    const ERROR_SUMMARY: u16 = 1 << 15;
    const DATA_TIMEOUT: u16 = 1 << 4;

    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());
    let (mut endpoint, _control) = host.take_irq_source().unwrap().into_parts();
    host.enable_completion_irq().unwrap();
    write_u16(
        core.base(),
        NORMAL_INT_STATUS,
        TRANSFER_COMPLETE | ERROR_SUMMARY,
    );
    write_u16(core.base(), ERROR_INT_STATUS, DATA_TIMEOUT);
    let rdif_block::IrqCapture::Captured { event, masked } =
        rdif_block::IrqEndpoint::capture(&mut endpoint)
    else {
        panic!("combined SDHCI status must be captured");
    };
    assert!(masked.is_none());

    assert_eq!(
        event,
        sdhci_host::Event::from_status(TRANSFER_COMPLETE | ERROR_SUMMARY, DATA_TIMEOUT)
    );
}

#[test]
fn recovery_rebuilds_board_power_pads_and_phy_before_ready() {
    const SOFTWARE_RESET: usize = 0x2f;

    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());
    let _source = host.take_irq_source().unwrap();
    host.enable_completion_irq().unwrap();
    host.disable_completion_irq().unwrap();
    let mut recovery = SdioHost2Lifecycle::begin_recovery(
        &mut host,
        rdif_block::RecoveryCause::QueueFault { queue_id: 0 },
    )
    .unwrap();

    let wake_at_ns = match SdioHost2Lifecycle::poll_dma_quiesce(
        &mut host,
        &mut recovery,
        rdif_block::InitInput::at(1_000),
    ) {
        rdif_block::InitPoll::Pending(schedule) => schedule.wake_at_ns().unwrap(),
        _ => panic!("first recovery pass must arm an absolute reset deadline"),
    };
    assert!(wake_at_ns > 1_000);
    write_u8(core.base(), SOFTWARE_RESET, 0);
    assert!(matches!(
        SdioHost2Lifecycle::poll_dma_quiesce(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at_ns),
        ),
        rdif_block::InitPoll::Ready(())
    ));

    write_u32(core.base(), CVI_VENDOR_MSHC_CTRL, 0);
    write_u32(core.base(), CVI_PHY_TX_RX_DLY, 0);
    write_u32(core.base(), CVI_PHY_CONFIG, 0);
    write_u32(syscon.base(), TOP_SD_PWRSW_CTRL, TOP_SD_PWRSW_OFF);
    SdioHost2Lifecycle::begin_reinitialize(&mut host, &mut recovery).unwrap();
    assert!(matches!(
        SdioHost2Lifecycle::poll_reinitialize(
            &mut host,
            &mut recovery,
            rdif_block::InitInput::at(wake_at_ns),
        ),
        rdif_block::InitPoll::Ready(())
    ));

    assert_eq!(
        read_u32(syscon.base(), TOP_SD_PWRSW_CTRL) & TOP_SD_PWRSW_LOW_MASK,
        TOP_SD_PWRSW_3V3
    );
    assert_eq!(
        read_u32(core.base(), CVI_VENDOR_MSHC_CTRL) & MSHC_CTRL_DS_HS_BITS,
        MSHC_CTRL_DS_HS_BITS
    );
    assert_eq!(
        read_u32(core.base(), CVI_PHY_TX_RX_DLY),
        PHY_TX_RX_DLY_DS_HS
    );
    assert_eq!(read_u32(core.base(), CVI_PHY_CONFIG), PHY_CONFIG_DS_HS);
}

#[test]
fn power_on_sequence_configures_3v3_pads_io_and_ds_hs_phy() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    write_u32(syscon.base(), TOP_SD_PWRSW_CTRL, 0xa5a5_a5a0);
    write_u8(
        unsafe { NonNull::new_unchecked(syscon.base().as_ptr().add(SYSCON_PINMUX_OFFSET)) },
        PINMUX_SDIO0_PWR_EN,
        0x7,
    );

    let mut host = new_host(
        &mut core,
        &mut syscon,
        Cv181xConfig {
            has_card_detect_gpio: true,
            ..Cv181xConfig::default()
        },
    );
    host.configure_sd_power_on();

    let pinmux =
        unsafe { NonNull::new_unchecked(syscon.base().as_ptr().add(SYSCON_PINMUX_OFFSET)) };
    assert_eq!(
        read_u32(syscon.base(), TOP_SD_PWRSW_CTRL),
        0xa5a5_a5a0 | TOP_SD_PWRSW_3V3
    );
    assert_eq!(read_u8(pinmux, PINMUX_SDIO0_CD), PINMUX_FUNC_XGPIO);
    assert_eq!(read_u8(pinmux, PINMUX_SDIO0_CLK), PINMUX_FUNC_SDIO0);
    assert_eq!(read_u8(pinmux, PINMUX_SDIO0_CMD), PINMUX_FUNC_SDIO0);
    assert_eq!(read_u8(pinmux, PINMUX_SDIO0_D3), PINMUX_FUNC_SDIO0);
    assert_eq!(read_u8(pinmux, PINMUX_SDIO0_PWR_EN), 0x7);
    assert_eq!(read_u8(pinmux, IO_SDIO0_CMD) & IO_PULL_UP, IO_PULL_UP);
    assert_eq!(read_u8(pinmux, IO_SDIO0_CMD) & IO_PULL_DOWN, 0);
    assert_eq!(
        read_u32(core.base(), CVI_PHY_TX_RX_DLY),
        PHY_TX_RX_DLY_DS_HS
    );
    assert_eq!(read_u32(core.base(), CVI_PHY_CONFIG), PHY_CONFIG_DS_HS);
    assert_eq!(
        read_u32(core.base(), CVI_VENDOR_MSHC_CTRL) & MSHC_CTRL_DS_HS_BITS,
        MSHC_CTRL_DS_HS_BITS
    );
}

#[test]
fn power_off_switches_sd_pads_to_gpio_and_closes_power() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());

    host.configure_sd_power_off();

    let pinmux =
        unsafe { NonNull::new_unchecked(syscon.base().as_ptr().add(SYSCON_PINMUX_OFFSET)) };
    assert_eq!(read_u8(pinmux, PINMUX_SDIO0_CLK), PINMUX_FUNC_XGPIO);
    assert_eq!(read_u8(pinmux, PINMUX_SDIO0_D0), PINMUX_FUNC_XGPIO);
    assert_eq!(read_u8(pinmux, IO_SDIO0_D0) & IO_PULL_DOWN, IO_PULL_DOWN);
    assert_eq!(
        read_u32(syscon.base(), TOP_SD_PWRSW_CTRL) & TOP_SD_PWRSW_LOW_MASK,
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

    let mut request = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetBusWidth(BusWidth::Bit4),
        )
    }
    .unwrap();

    assert_eq!(
        poll_ready_bus_op(&mut host, &mut request),
        Err(sdio_host2::Error::Unsupported)
    );
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
        host.clock_plan(ClockSpeed::Sdr50),
        Err(sdio_host2::Error::Unsupported)
    );

    let mut request = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V180),
        )
    }
    .unwrap();

    assert_eq!(
        poll_ready_bus_op(&mut host, &mut request),
        Err(sdio_host2::Error::Unsupported)
    );
}

#[test]
fn high_speed_mode_sets_host_timing_even_when_clock_is_capped() {
    let mut core = FakeMmio::new();
    let mut syscon = FakeMmio::new();
    let mut host = new_host(&mut core, &mut syscon, Cv181xConfig::default());

    let _request = unsafe {
        sdio_host2::SdioHost::submit_bus_op(
            &mut host,
            sdio_host2::BusOp::SetClock(ClockSpeed::HighSpeed),
        )
    }
    .unwrap();

    assert_eq!(
        read_u8(core.base(), REG_HOST_CONTROL1) & HOST_CTRL1_HIGH_SPEED,
        HOST_CTRL1_HIGH_SPEED
    );
    assert_eq!(
        read_u16(core.base(), REG_HOST_CONTROL2) & HOST_CTRL2_UHS_MODE_MASK,
        HOST_CTRL2_UHS_SDR25
    );
}

#[test]
fn wrapper_exposes_timed_initialization_and_typed_recovery() {
    fn assert_runtime_contract<T>()
    where
        T: sdmmc_protocol::sdio::host2::SdioHost2Timed
            + sdmmc_protocol::sdio::host2::SdioHost2Lifecycle,
    {
    }

    assert_runtime_contract::<Cv181xSdhci>();
}
