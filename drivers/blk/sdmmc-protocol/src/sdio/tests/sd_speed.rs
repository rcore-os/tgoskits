use super::*;

#[test]
fn sd_init_automatically_selects_sdr104_when_card_and_host_agree() {
    let mut replies = sd_init_replies_with_ocr(ocr_ready_sdhc_s18a());
    replies.extend([
        Ok(ok_r1()),         // CMD6 query access modes
        Ok(ok_r1()),         // CMD11 voltage switch command
        Ok(ok_r1()),         // CMD6 switch SDR104
        Ok(r1_tran_ready()), // CMD13 verify
    ]);
    let mut host = MockHost::with_results(replies);
    host.read_payloads = std::vec![
        switch_status_payload(0, 1 << 3),
        switch_status_payload(3, 1 << 3),
    ];

    let mut driver = SdioSdmmc::new(host);
    poll_init_to_completion(&mut driver).expect("SD init succeeds with SDR104");

    assert_eq!(driver.host().last_voltage, Some(SignalVoltage::V180));
    assert_eq!(driver.host().last_clock, Some(ClockSpeed::Sdr104));
    assert_eq!(
        driver.host().last_tuning,
        Some((19, crate::cmd::SD_TUNING_BLOCK_SIZE as u16))
    );
    assert!(
        driver.host().commands.iter().any(|c| c.index == 11),
        "CMD11 issued before host voltage switch"
    );
    assert!(
        driver
            .host()
            .commands
            .iter()
            .any(|c| c.index == 6 && c.argument == 0x80FF_FFF3),
        "CMD6 switched group 1 to SDR104"
    );
}

#[test]
fn sd_init_can_limit_speed_selection_to_legacy_high_speed() {
    let mut replies = sd_init_replies_with_ocr(ocr_ready_sdhc_s18a());
    replies.extend([
        Ok(ok_r1()),         // CMD6 query access modes
        Ok(ok_r1()),         // CMD6 switch HighSpeed
        Ok(r1_tran_ready()), // CMD13 verify
    ]);
    let mut host = MockHost::with_results(replies);
    host.read_payloads = std::vec![
        switch_status_payload(0, (1 << 3) | (1 << 1)),
        switch_status_payload(1, (1 << 3) | (1 << 1)),
    ];

    let mut driver = SdioSdmmc::new(host);
    driver.set_sd_uhs_selection_enabled(false);
    poll_init_to_completion(&mut driver)
        .expect("SD init selects legacy HighSpeed without trying UHS");

    assert!(
        !driver
            .host()
            .events
            .iter()
            .any(|e| matches!(e, MockEvent::Voltage(SignalVoltage::V180))),
        "legacy-HighSpeed init must never ask the host for 1.8 V"
    );
    assert_eq!(driver.host().last_clock, Some(ClockSpeed::HighSpeed));
    assert_eq!(driver.host().last_tuning, None);
    assert!(
        !driver.host().commands.iter().any(|c| c.index == 11),
        "CMD11 voltage switch must not be issued in legacy HighSpeed-only mode"
    );
    assert!(
        driver
            .host()
            .commands
            .iter()
            .any(|c| c.index == 6 && c.argument == 0x80FF_FFF1),
        "CMD6 switched group 1 to HighSpeed"
    );
    assert!(
        !driver
            .host()
            .commands
            .iter()
            .any(|c| c.index == 6 && c.argument == 0x80FF_FFF3),
        "SDR104 must not be selected in legacy HighSpeed-only mode"
    );
}

#[test]
fn sd_init_falls_back_to_high_speed_when_uhs_voltage_switch_fails() {
    let mut replies = sd_init_replies_with_ocr(ocr_ready_sdhc_s18a());
    replies.extend([
        Ok(ok_r1()),         // CMD6 query access modes
        Ok(ok_r1()),         // CMD11 voltage switch command
        Ok(ok_r1()),         // CMD6 switch HighSpeed
        Ok(r1_tran_ready()), // CMD13 verify
    ]);
    let mut host = MockHost::with_results(replies);
    host.read_payloads = std::vec![
        switch_status_payload(0, (1 << 3) | (1 << 1)),
        switch_status_payload(1, 1 << 1),
    ];
    host.voltage_switch_result = Some(Error::UnsupportedCommand);

    let mut driver = SdioSdmmc::new(host);
    poll_init_to_completion(&mut driver).expect("SD init falls back when UHS voltage switch fails");

    assert_eq!(driver.host().last_voltage, Some(SignalVoltage::V180));
    assert_eq!(driver.host().last_clock, Some(ClockSpeed::HighSpeed));
    assert_eq!(driver.host().last_tuning, None);
    assert!(
        driver
            .host()
            .commands
            .iter()
            .any(|c| c.index == 6 && c.argument == 0x80FF_FFF1),
        "CMD6 switched group 1 to HighSpeed after UHS fallback"
    );
}

#[test]
fn init_voltage_reset_only_ignores_unsupported() {
    let mut host = MockHost::with_results(Vec::new());
    host.voltage_switch_result = Some(Error::Busy);
    let mut driver = SdioSdmmc::new(host);
    let mut request = driver.submit_init().unwrap();

    for _ in 0..4 {
        assert!(matches!(
            advance_init_once(&mut driver, &mut request).unwrap(),
            OperationProgress::Pending
        ));
    }
    assert!(matches!(
        advance_init_once(&mut driver, &mut request),
        Err(Error::Busy)
    ));
    assert!(matches!(request.state, SdioInitState::ResetVoltage));
}

#[test]
fn sd_speed_selection_can_be_disabled_for_default_speed_bringup() {
    let replies = sd_init_replies_with_ocr(ocr_ready_sdhc_s18a());
    let host = MockHost::with_results(replies);
    let mut driver = SdioSdmmc::new(host);
    driver.set_sd_speed_selection_enabled(false);

    poll_init_to_completion(&mut driver).expect("SD init succeeds without CMD6 speed switching");

    assert_eq!(driver.host().bus_width, Some(BusWidth::Bit4));
    assert_eq!(driver.host().last_clock, Some(ClockSpeed::Default));
    assert!(
        driver
            .host()
            .commands
            .iter()
            .filter(|c| c.index == 6)
            .all(|c| c.argument == 2),
        "only ACMD6 bus-width switch is issued; no CMD6 SWITCH_FUNC"
    );
    assert!(
        !driver
            .host()
            .events
            .iter()
            .any(|e| matches!(e, MockEvent::Voltage(SignalVoltage::V180))),
        "speed-selection-disabled init must never ask the host for 1.8 V"
    );
    assert_eq!(driver.host().last_tuning, None);
}

#[test]
fn sd_init_keeps_default_speed_when_switch_function_is_unsupported() {
    let replies = sd_init_replies_with_ocr(ocr_ready_sdhc_s18a());
    let host = MockHost::with_results(replies);
    let mut driver = SdioSdmmc::new(host);

    poll_init_to_completion(&mut driver)
        .expect("optional CMD6 rejection must not fail SD initialization");

    assert_eq!(driver.host().bus_width, Some(BusWidth::Bit4));
    assert_eq!(driver.host().last_clock, Some(ClockSpeed::Default));
    assert_eq!(driver.host().last_tuning, None);
}
