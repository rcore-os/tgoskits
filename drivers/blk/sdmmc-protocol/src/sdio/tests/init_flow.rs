use super::*;

/// When init fails mid-flight after the driver has already negotiated
/// past identification mode (e.g. host switched to 4-bit, raised clock
/// to Default), the driver must reset the host back to a clean baseline
/// (1-bit, identification clock, 3.3 V signaling) so a caller retry from
/// `submit_init` starts on solid ground. Without this, a later CMD0
/// would be issued over a bus configured for a card that just failed.
#[test]
fn poll_init_request_resets_host_when_card_init_fails() {
    // SD init runs through CMD0 → CMD8 → ACMD41 → CMD2 → CMD3 → CMD9 →
    // CMD7 → CMD55 → ACMD6 (host now at 4-bit + Default clock), then
    // PrepareSdSpeed issues a 64-byte CMD6 SWITCH_FUNC. We feed it a
    // valid switch-status payload so the read completes, then poison
    // the *next* reply with OUT_OF_RANGE so the protocol layer raises
    // Err on PollSdSetAccessMode's R1 — long after the host left
    // identification mode.
    let mut replies = sd_init_replies_with_ocr(ocr_ready_sdhc());
    // After ACMD6: CMD6 SWITCH_FUNC query (R1 + 64B data) succeeds.
    replies.push(Ok(ok_r1()));
    // Then the access-mode switch CMD6 returns a poisoned R1 with
    // OUT_OF_RANGE; protocol surfaces Err(CardError::OutOfRange).
    replies.push(Ok(Response::R1(R1Response { raw: 1 << 31 })));
    let mut host = MockHost::with_results(replies);
    // SwitchStatus payload advertising HighSpeed (function 1, bit 1
    // supported in group 1). Used for both CMD6 reads.
    host.read_payloads = std::vec![
        switch_status_payload(0, 1 << 1),
        switch_status_payload(1, 1 << 1),
    ];
    let mut driver = SdioSdmmc::new(host);

    let err =
        poll_init_to_completion(&mut driver).expect_err("init must propagate the injected failure");
    // Exact error type isn't load-bearing; what matters is that the
    // abort_init path ran on the failure.
    let _ = err;

    // After the abort path runs, the host must be back at 1-bit /
    // identification clock / 3.3 V signaling. The driver also clears its
    // cached card state so a retry from submit_init is well-defined.
    assert_eq!(driver.host.bus_width, Some(BusWidth::Bit1));
    assert_eq!(driver.host.last_clock, Some(ClockSpeed::Identification));
    assert_eq!(driver.host.last_voltage, Some(SignalVoltage::V330));
    assert_eq!(driver.rca(), 0);
    assert!(!driver.is_high_capacity());
}

#[test]
fn init_records_rca_in_driver_state() {
    let replies = sd_init_replies();
    let host = MockHost::with_results(replies);
    let mut driver = SdioSdmmc::new(host);
    disable_speed_selection(&mut driver);
    let info = poll_init_to_completion(&mut driver).unwrap();

    assert_eq!(info.rca, 0x1234);
    assert_eq!(driver.rca(), 0x1234);
    assert!(info.high_capacity);
    assert_eq!(info.kind, CardKind::Sd);
    assert_eq!(info.capacity_blocks, Some((0x0F0F + 1) * 1024));
    let cid = info.cid.expect("CID captured in init");
    assert_eq!(cid.manufacturer_id(), 0x03);
    assert_eq!(&cid.product_name(), b"ABC12");
    assert_eq!(driver.host.bus_width, Some(BusWidth::Bit4));

    // Verify CMD7 / CMD55 / ACMD6 used the recorded RCA, not 0.
    let cmd7 = driver
        .host
        .commands
        .iter()
        .find(|c| c.index == 7)
        .expect("CMD7 issued");
    assert_eq!(cmd7.argument, (0x1234u32) << 16);
}

#[test]
fn submit_init_starts_request_without_spinning_past_pending_cmd0() {
    let mut host = MockHost::with_results(std::vec![Ok(ok_r1())]);
    host.pending_polls = 1;
    let mut driver = SdioSdmmc::new(host);
    let mut scratch = SdioInitScratch::new();
    let mut request = driver.submit_init(&mut scratch).unwrap();

    assert!(driver.host.commands.is_empty());
    for _ in 0..16 {
        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        let _ = request.take_needs_pace();
        if !driver.host.commands.is_empty() {
            break;
        }
    }
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![0]
    );
    assert!(matches!(
        driver.poll_init_request(&mut request).unwrap(),
        OperationPoll::Pending
    ));
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![0]
    );
}

#[test]
fn poll_init_request_returns_after_submitting_next_command() {
    let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![
        Ok(ok_r1()),                                             // CMD0
        Ok(Response::R7(IfCondResponse::from_raw(0x0000_01AA))), // CMD8
    ]));
    let mut scratch = SdioInitScratch::new();
    let mut request = driver.submit_init(&mut scratch).unwrap();

    for _ in 0..10 {
        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
    }
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![0, 8]
    );

    assert!(matches!(
        driver.poll_init_request(&mut request).unwrap(),
        OperationPoll::Pending
    ));
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![0, 8, 55]
    );
}

#[test]
fn poll_init_request_falls_back_to_cmd1_after_acmd41_not_ready_timeout() {
    let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![
        Ok(Response::R3(OcrResponse::from_raw(0x00FF_8000))),
        Ok(ok_r1()),
    ]));
    let mut scratch = SdioInitScratch::new();
    let mut request = SdioInitRequest::new(CardInitPreference::SdFirst, &mut scratch);
    request.state = SdioInitState::PollAcmd41;
    request.sd_v2 = false;
    request.acmd41_polls = SdioInitTiming::MAX_POLLS;

    assert!(matches!(
        driver.poll_init_request(&mut request).unwrap(),
        OperationPoll::Pending
    ));
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![1]
    );
}

#[test]
fn poll_init_request_sd_only_does_not_fallback_to_cmd1_after_acmd41_timeout() {
    let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![Ok(Response::R3(
        OcrResponse::from_raw(0x00FF_8000),
    ))]));
    let mut scratch = SdioInitScratch::new();
    let mut request = SdioInitRequest::new(CardInitPreference::SdOnly, &mut scratch);
    request.state = SdioInitState::PollAcmd41;
    request.sd_v2 = false;
    request.acmd41_polls = SdioInitTiming::MAX_POLLS;

    assert!(matches!(
        driver.poll_init_request(&mut request),
        Err(Error::Timeout(_))
    ));
    assert!(driver.host.commands.is_empty());
}

#[test]
fn submit_init_with_mmc_preference_skips_sd_probe_after_cmd0() {
    let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![Ok(ok_r1())]));
    let mut scratch = SdioInitScratch::new();
    let mut request = driver
        .submit_init_with_preference(CardInitPreference::MmcFirst, &mut scratch)
        .unwrap();

    for _ in 0..16 {
        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        let _ = request.take_needs_pace();
        if driver.host.commands.iter().any(|cmd| cmd.index == 1) {
            break;
        }
    }
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![0, 1]
    );
}

#[test]
fn submit_mmc_switch_returns_before_polling_status() {
    let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![
        Ok(ok_r1()),         // CMD6
        Ok(r1_tran_ready()), // CMD13
    ]));
    driver.rca = 1;

    let mut request = driver
        .submit_mmc_switch(0b11, crate::cmd::ext_csd::HS_TIMING as u8, 1)
        .unwrap();
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![6]
    );

    assert!(matches!(
        driver.poll_mmc_switch_request(&mut request).unwrap(),
        OperationPoll::Pending
    ));
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![6, 13]
    );

    assert!(matches!(
        driver.poll_mmc_switch_request(&mut request).unwrap(),
        OperationPoll::Complete(())
    ));
}

#[test]
fn mmc_switch_surfaces_wall_clock_timeout_when_host_has_clock() {
    // Programming-state R1: READY_FOR_DATA (bit 8) + state nibble 7
    // (bits 9..=12). The mmc_switch loop will keep retrying until either
    // MAX_POLLS or TIMEOUT_MS trips.
    let programming = || -> Response {
        Response::R1(R1Response::from_native_raw((1u32 << 8) | (7u32 << 9)).unwrap())
    };

    let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![
        Ok(ok_r1()),       // CMD6 ack
        Ok(programming()), // CMD13 #1
        Ok(programming()), // CMD13 #2
    ]));
    driver.rca = 1;
    // Arm the clock at t=0 so submit_mmc_switch records started_ms=0.
    driver.host.now_ms = Some(0);

    let mut request = driver
        .submit_mmc_switch(0b11, crate::cmd::ext_csd::HS_TIMING as u8, 1)
        .unwrap();
    // 1st poll: CMD6 ack, schedule CMD13.
    assert!(matches!(
        driver.poll_mmc_switch_request(&mut request).unwrap(),
        OperationPoll::Pending
    ));
    // 2nd poll: CMD13 says still programming; well within the wall-clock
    // budget, so the loop reissues CMD13.
    assert!(matches!(
        driver.poll_mmc_switch_request(&mut request).unwrap(),
        OperationPoll::Pending
    ));
    let polls_before_jump = request.polls;
    assert!(polls_before_jump < MmcSwitchTiming::MAX_POLLS);

    // Jump the wall clock past the 250 ms CMD6 SWITCH budget.
    driver.host.now_ms = Some(MmcSwitchTiming::TIMEOUT_MS + 1);

    // 3rd poll: CMD13 still reports programming, but the wall-clock
    // deadline fires before the poll counter would have.
    let err = driver.poll_mmc_switch_request(&mut request).unwrap_err();
    assert!(
        matches!(err, Error::Timeout(ctx) if ctx.cmd == Some(6)),
        "expected CMD6 timeout, got {:?}",
        err
    );
    assert!(
        request.polls < MmcSwitchTiming::MAX_POLLS,
        "wall-clock check should fire before the poll budget ({} < {})",
        request.polls,
        MmcSwitchTiming::MAX_POLLS
    );
}

#[test]
fn submit_status_returns_before_polling_cmd13_response() {
    let mut driver = SdioSdmmc::new(MockHost::with_results(std::vec![Ok(r1_tran_ready())]));
    driver.rca = 0x1234;

    let mut request = driver.submit_status().unwrap();
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![13]
    );
    assert_eq!(driver.host.commands[0].argument, 0x1234 << 16);

    assert!(matches!(
        driver.poll_status_request(&mut request).unwrap(),
        OperationPoll::Complete(CardState::Transfer)
    ));
}

#[test]
fn submit_read_ext_csd_uses_caller_buffer_and_poll_completion() {
    let mut host = MockHost::new(std::vec![ok_r1()]);
    let payload = ext_csd_blob();
    host.next_read_payload = Some(payload.clone());
    let mut driver = SdioSdmmc::new(host);
    let mut buf = [0u8; 512];

    let mut request = driver.submit_read_ext_csd(&mut buf).unwrap();
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![8]
    );

    assert!(matches!(
        driver.poll_ext_csd_request(&mut request).unwrap(),
        OperationPoll::Complete(())
    ));
    drop(request);
    assert_eq!(&buf[..], payload.as_slice());
}

#[test]
fn submit_switch_function_uses_caller_buffer_and_poll_completion() {
    let mut host = MockHost::new(std::vec![ok_r1()]);
    let payload = switch_status_payload(1, 1 << 1);
    host.next_read_payload = Some(payload.clone());
    let mut driver = SdioSdmmc::new(host);
    let mut buf = [0u8; 64];

    let mut request = driver
        .submit_switch_function(&crate::cmd::cmd6_high_speed(true), &mut buf)
        .unwrap();
    assert_eq!(
        driver
            .host
            .commands
            .iter()
            .map(|cmd| cmd.index)
            .collect::<Vec<_>>(),
        std::vec![6]
    );

    assert!(matches!(
        driver.poll_switch_function_request(&mut request).unwrap(),
        OperationPoll::Complete(())
    ));
    drop(request);
    assert_eq!(&buf[..], payload.as_slice());
}

#[test]
fn poll_init_request_ready_path_only_uses_linux_power_on_pace_hints() {
    let replies = sd_init_replies();
    let host = MockHost::with_results(replies);
    let mut driver = SdioSdmmc::new(host);
    disable_speed_selection(&mut driver);
    let mut scratch = SdioInitScratch::new();
    let mut request = driver.submit_init(&mut scratch).unwrap();
    let mut pace_hints = 0;
    let info = loop {
        match driver.poll_init_request(&mut request).unwrap() {
            OperationPoll::Pending => {
                if request.take_needs_pace() {
                    pace_hints += 1;
                }
            }
            OperationPoll::Complete(info) => break info,
        }
    };

    assert_eq!(info.rca, 0x1234);
    assert_eq!(
        pace_hints, 2,
        "ready card path should only pace for Linux-style power stabilization, not for \
         ACMD41/CMD1 retries"
    );
}

#[test]
fn poll_init_request_paces_after_power_on_before_clocking_card() {
    let host = MockHost::with_results(std::vec![Ok(ok_r1())]);
    let mut driver = SdioSdmmc::new(host);
    let mut scratch = SdioInitScratch::new();
    let mut request = driver.submit_init(&mut scratch).unwrap();

    for _ in 0..4 {
        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
    }

    assert!(
        driver.host.commands.is_empty(),
        "no card command should be issued before the post-power-on pace point"
    );
    assert!(
        request.take_needs_pace(),
        "init must wait after bus power-on before driving more commands, matching Linux \
         mmc_power_up()"
    );
}

#[test]
fn poll_init_request_paces_after_identification_clock_before_cmd0() {
    let host = MockHost::with_results(std::vec![Ok(ok_r1())]);
    let mut driver = SdioSdmmc::new(host);
    let mut scratch = SdioInitScratch::new();
    let mut request = driver.submit_init(&mut scratch).unwrap();

    loop {
        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        let needs_pace = request.take_needs_pace();
        if driver.host.last_clock == Some(ClockSpeed::Identification) && needs_pace {
            break;
        }
    }

    assert!(
        driver.host.commands.is_empty(),
        "CMD0 must wait until the post-identification-clock pace point has elapsed"
    );
}

#[test]
fn poll_init_request_sets_pace_hint_for_power_up_retry() {
    let replies = std::vec![
        Ok(ok_r1()),                                             // CMD0
        Ok(Response::R7(IfCondResponse::from_raw(0x0000_01AA))), // CMD8
        Ok(ok_r1()),                                             // CMD55
        Ok(Response::R3(OcrResponse::from_raw(0x00FF_8000))),    // ACMD41 not ready
        Ok(ok_r1()),                                             // CMD55
        Ok(ocr_ready_sdhc()),                                    // ACMD41 ready
        Ok(cid_response()),                                      // CMD2
        Ok(rca_response(0x1234)),                                // CMD3
        Ok(csd_v2_response()),                                   // CMD9
        Ok(ok_r1()),                                             // CMD7
        Ok(ok_r1()),                                             // CMD55
        Ok(ok_r1()),                                             // ACMD6
    ];
    let host = MockHost::with_results(replies);
    let mut driver = SdioSdmmc::new(host);
    disable_speed_selection(&mut driver);
    let mut scratch = SdioInitScratch::new();
    let mut request = driver.submit_init(&mut scratch).unwrap();
    let mut pace_hints = 0;
    let info = loop {
        match driver.poll_init_request(&mut request).unwrap() {
            OperationPoll::Pending => {
                if request.take_needs_pace() {
                    pace_hints += 1;
                }
            }
            OperationPoll::Complete(info) => break info,
        }
    };

    assert_eq!(info.rca, 0x1234);
    assert_eq!(
        pace_hints, 3,
        "two Linux-style power-up pace points plus one ACMD41 retry pace"
    );
}
