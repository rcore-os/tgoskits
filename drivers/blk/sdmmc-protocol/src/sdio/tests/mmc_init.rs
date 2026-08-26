use super::*;

#[test]
fn init_falls_back_to_mmc_when_cmd8_and_acmd41_fail() {
    // Canonical eMMC bring-up: CMD8 returns nothing (host reports
    // timeout), ACMD41 also fails (eMMC ignores it), then CMD1 takes
    // over and reports the card ready immediately. After CMD7 the
    // driver reads EXT_CSD, then issues CMD6 SWITCH twice (8-bit
    // bus width, HS_TIMING=1) — each followed by CMD13 polling for
    // tran state.
    let replies = std::vec![
        Ok(ok_r1()),                // CMD0
        cmd8_timeout(),             // CMD8 — eMMC ignores
        Ok(ok_r1()),                // CMD55 (ACMD41 prologue)
        acmd41_timeout(),           // ACMD41 — eMMC ignores
        Ok(ocr_ready_mmc_sector()), // CMD1 — card reports ready
        Ok(cid_response()),         // CMD2
        Ok(ok_r1()),                // CMD3 (host-assigned RCA, R1 ack)
        Ok(csd_v2_response()),      // CMD9
        Ok(ok_r1()),                // CMD7 (select)
        Ok(ok_r1()),                // CMD8 MMC SEND_EXT_CSD — R1 (data follows)
        Ok(ok_r1()),                // CMD6 SWITCH — BUS_WIDTH=2 (8-bit)
        Ok(r1_tran_ready()),        // CMD13 — tran + ready
        Ok(ok_r1()),                // CMD6 SWITCH — HS_TIMING=1
        Ok(r1_tran_ready()),        // CMD13 — tran + ready
    ];
    let mut host = MockHost::with_results(replies);
    host.next_read_payload = Some(ext_csd_blob());
    let mut driver = SdMmcCard::new(host);
    let info = poll_init_to_completion(&mut driver).expect("eMMC init succeeds");

    assert_eq!(info.kind, CardKind::Mmc);
    assert_eq!(driver.kind(), CardKind::Mmc);
    assert!(!info.sd_v2);
    assert!(info.high_capacity, "OCR bit 30 set → sector mode");
    assert_eq!(info.rca, 1);
    // Capacity should come from EXT_CSD.SEC_COUNT, not the legacy CSD.
    assert_eq!(info.capacity_blocks, Some(0x0080_0000));
    // EXT_CSD got captured.
    assert!(info.ext_csd.is_some());

    let cmds = &driver.host().commands;
    let cmd3 = cmds.iter().find(|c| c.index == 3).expect("CMD3 issued");
    assert_eq!(cmd3.argument, 1u32 << 16);
    assert!(cmds.iter().any(|c| c.index == 1), "CMD1 issued");

    // Two CMD6 SWITCHes — one for BUS_WIDTH, one for HS_TIMING.
    let cmd6s: Vec<&Command> = cmds.iter().filter(|c| c.index == 6).collect();
    assert_eq!(cmd6s.len(), 2, "two CMD6 SWITCHes (BUS_WIDTH + HS_TIMING)");
    // First: WRITE_BYTE | BUS_WIDTH(183) | value=2 (8-bit)
    let bw_arg = (0b11u32 << 24) | ((183u32) << 16) | (2u32 << 8);
    assert_eq!(cmd6s[0].argument, bw_arg, "BUS_WIDTH=8-bit");
    // Second: WRITE_BYTE | HS_TIMING(185) | value=1 (HS)
    let hs_arg = (0b11u32 << 24) | ((185u32) << 16) | (1u32 << 8);
    assert_eq!(cmd6s[1].argument, hs_arg, "HS_TIMING=1");

    // Host should have ended up at 8-bit (Bit8 was accepted).
    assert_eq!(driver.host().bus_width, Some(BusWidth::Bit8));
}

#[test]
fn mmc_init_enables_an_advertised_write_cache_before_completion() {
    let replies = std::vec![
        Ok(ok_r1()),                // CMD0
        cmd8_timeout(),             // CMD8
        Ok(ok_r1()),                // CMD55
        acmd41_timeout(),           // ACMD41
        Ok(ocr_ready_mmc_sector()), // CMD1
        Ok(cid_response()),         // CMD2
        Ok(ok_r1()),                // CMD3
        Ok(csd_v2_response()),      // CMD9
        Ok(ok_r1()),                // CMD7
        Ok(ok_r1()),                // CMD8 MMC
        Ok(ok_r1()),                // CMD6 BUS_WIDTH=8
        Ok(r1_tran_ready()),        // CMD13
        Ok(ok_r1()),                // CMD6 HS_TIMING=1
        Ok(r1_tran_ready()),        // CMD13
        Ok(ok_r1()),                // CMD6 CACHE_CTRL=1
        Ok(r1_tran_ready()),        // CMD13
    ];
    let mut host = MockHost::with_results(replies);
    let mut ext_csd = ext_csd_blob();
    ext_csd[crate::cmd::ext_csd::REV] = 6;
    let cache_size = 1024u32;
    let offset = crate::cmd::ext_csd::CACHE_SIZE;
    ext_csd[offset..offset + 4].copy_from_slice(&cache_size.to_le_bytes());
    host.next_read_payload = Some(ext_csd);

    let mut driver = SdMmcCard::new(host);
    let info = poll_init_to_completion(&mut driver).expect("eMMC cache enable succeeds");

    let ext_csd = info.ext_csd.expect("MMC init returns EXT_CSD");
    assert_eq!(ext_csd.cache_size_kib(), cache_size);
    assert!(ext_csd.cache_enabled());
    let cache_enable_argument =
        (0b11u32 << 24) | ((crate::cmd::ext_csd::CACHE_CTRL as u32) << 16) | (1u32 << 8);
    assert!(
        driver
            .host()
            .commands
            .iter()
            .any(|command| command.index == 6 && command.argument == cache_enable_argument),
        "CACHE_CTRL must be enabled before initialization completes"
    );
}

#[test]
fn mmc_init_falls_back_to_4bit_when_host_refuses_8bit() {
    // Same as the canonical path but the host's set_bus_width
    // rejects Bit8. The driver must retry with Bit4 and end up
    // settled there, not silently leave the card at 8-bit.
    let replies = std::vec![
        Ok(ok_r1()),                // CMD0
        cmd8_timeout(),             // CMD8
        Ok(ok_r1()),                // CMD55
        acmd41_timeout(),           // ACMD41
        Ok(ocr_ready_mmc_sector()), // CMD1
        Ok(cid_response()),         // CMD2
        Ok(ok_r1()),                // CMD3
        Ok(csd_v2_response()),      // CMD9
        Ok(ok_r1()),                // CMD7
        Ok(ok_r1()),                // CMD8 MMC (R1)
        Ok(ok_r1()),                // CMD6 SWITCH (8-bit)
        Ok(r1_tran_ready()),        // CMD13 — tran (card *did* switch)
        // host.set_bus_width(Bit8) returns UnsupportedCommand, so the
        // driver retries with Bit4. No additional CMD6 needed for
        // the current implementation? Actually, yes — set_bus_width_mmc
        // re-issues CMD6 with BUS_WIDTH=1 first.
        Ok(ok_r1()),         // CMD6 SWITCH (4-bit)
        Ok(r1_tran_ready()), // CMD13 — tran
        Ok(ok_r1()),         // CMD6 SWITCH (HS_TIMING=1)
        Ok(r1_tran_ready()), // CMD13 — tran
    ];
    let mut host = MockHost::with_results(replies);
    host.next_read_payload = Some(ext_csd_blob());
    host.reject_bit8 = true;
    let mut driver = SdMmcCard::new(host);
    let _info =
        poll_init_to_completion(&mut driver).expect("eMMC init succeeds with 4-bit fallback");

    assert_eq!(driver.host().bus_width, Some(BusWidth::Bit4));
}

#[test]
fn init_treats_sd_v1_correctly_when_cmd8_times_out_but_acmd41_succeeds() {
    // SD v1 cards (legacy SDSC) don't recognize CMD8 either, but
    // *do* answer ACMD41. The driver must not promote them to MMC
    // just because CMD8 timed out.
    let replies = std::vec![
        Ok(ok_r1()),    // CMD0
        cmd8_timeout(), // CMD8 — SD v1 no echo
        Ok(ok_r1()),    // CMD55 (ACMD41 prologue)
        // bit 31 set, bit 30 clear → SDSC, ready
        Ok(Response::R3(OcrResponse::from_raw(0x80FF_8000))),
        Ok(cid_response()),       // CMD2
        Ok(rca_response(0x4321)), // CMD3 (R6, card picks)
        Ok(csd_v2_response()),    // CMD9
        Ok(ok_r1()),              // CMD7
        Ok(ok_r1()),              // CMD55 (ACMD6 prologue)
        Ok(ok_r1()),              // ACMD6
    ];
    let host = MockHost::with_results(replies);
    let mut driver = SdMmcCard::new(host);
    disable_speed_selection(&mut driver);
    let info = poll_init_to_completion(&mut driver).expect("SD v1 init succeeds");

    assert_eq!(info.kind, CardKind::Sd, "ACMD41 success → SD, not MMC");
    assert!(!info.sd_v2);
    assert!(!info.high_capacity);
    assert_eq!(info.rca, 0x4321);
    assert_eq!(driver.host().bus_width, Some(BusWidth::Bit4));
}

/// Build an EXT_CSD payload that *also* advertises HS200 @ 1.8 V.
fn ext_csd_blob_hs200() -> Vec<u8> {
    use crate::cmd::ext_csd as e;
    let mut buf = ext_csd_blob();
    // OR in HS200_18V on top of HS_26 | HS_52 already present.
    buf[e::DEVICE_TYPE] |= e::device_type::HS200_18V;
    buf
}

#[test]
fn mmc_init_picks_hs200_when_card_and_host_agree() {
    // Sequence after CMD7:
    //   CMD8_MMC (R1) + 512B EXT_CSD
    //   CMD6 BUS_WIDTH=8 + CMD13 ready
    //   try_hs200:
    //     switch_voltage(V180)            ← host hook
    //     CMD6 HS_TIMING=0x02 + CMD13 ready
    //     set_clock(Hs200)                ← host hook
    //     execute_tuning(21)              ← host hook
    //     CMD13 ready (final verify)
    let replies = std::vec![
        Ok(ok_r1()),                // CMD0
        cmd8_timeout(),             // CMD8
        Ok(ok_r1()),                // CMD55
        acmd41_timeout(),           // ACMD41
        Ok(ocr_ready_mmc_sector()), // CMD1
        Ok(cid_response()),         // CMD2
        Ok(ok_r1()),                // CMD3
        Ok(csd_v2_response()),      // CMD9
        Ok(ok_r1()),                // CMD7
        Ok(ok_r1()),                // CMD8 MMC R1
        Ok(ok_r1()),                // CMD6 SWITCH BUS_WIDTH=8
        Ok(r1_tran_ready()),        // CMD13
        Ok(ok_r1()),                // CMD6 SWITCH HS_TIMING=2 (HS200)
        Ok(r1_tran_ready()),        // CMD13 (post-switch)
        Ok(r1_tran_ready()),        // CMD13 (HS200 verify)
    ];
    let mut host = MockHost::with_results(replies);
    host.next_read_payload = Some(ext_csd_blob_hs200());
    let mut driver = SdMmcCard::new(host);
    let _info = poll_init_to_completion(&mut driver).expect("HS200 init succeeds");

    // HS_TIMING write should carry value 0x02, not 0x01.
    let cmd6s: Vec<&Command> = driver
        .host()
        .commands
        .iter()
        .filter(|c| c.index == 6)
        .collect();
    // Two CMD6: BUS_WIDTH(=2) and HS_TIMING(=2)
    assert_eq!(cmd6s.len(), 2);
    let hs_timing_arg = (0b11u32 << 24) | ((185u32) << 16) | (0x02u32 << 8);
    assert_eq!(cmd6s[1].argument, hs_timing_arg, "HS_TIMING=2 (HS200)");

    // Host hooks were exercised.
    assert_eq!(driver.host().last_voltage, Some(SignalVoltage::V180));
    assert_eq!(driver.host().last_clock, Some(ClockSpeed::Hs200));
    assert_eq!(
        driver.host().last_tuning,
        Some((21, crate::cmd::MMC_TUNING_BLOCK_SIZE_8BIT as u16))
    );

    let hs200_clock_pos = driver
        .host()
        .events
        .iter()
        .position(|event| matches!(event, MockEvent::Clock(ClockSpeed::Hs200)))
        .expect("host clock is raised to HS200");
    let hs200_switch_pos = driver
        .host()
        .events
        .iter()
        .position(|event| {
            matches!(
                event,
                MockEvent::Command(Command {
                    index: 6,
                    argument,
                    ..
                }) if *argument == hs_timing_arg
            )
        })
        .expect("HS_TIMING=2 is programmed");
    assert!(
        hs200_switch_pos < hs200_clock_pos,
        "EXT_CSD HS_TIMING=2 must be programmed before raising host clock to HS200"
    );
}

#[test]
fn mmc_init_falls_back_to_hs52_when_tuning_fails() {
    // Card advertises HS200 + HS @ 52 MHz, but the host's
    // execute_tuning rejects (e.g. controller couldn't lock onto a
    // sampling phase). The driver must then re-enter the HS @ 52
    // MHz path: CMD6 HS_TIMING=1 + set_clock(HighSpeed). The card
    // ends up in HighSpeed, not Hs200.
    let replies = std::vec![
        Ok(ok_r1()),                // CMD0
        cmd8_timeout(),             // CMD8
        Ok(ok_r1()),                // CMD55
        acmd41_timeout(),           // ACMD41
        Ok(ocr_ready_mmc_sector()), // CMD1
        Ok(cid_response()),         // CMD2
        Ok(ok_r1()),                // CMD3
        Ok(csd_v2_response()),      // CMD9
        Ok(ok_r1()),                // CMD7
        Ok(ok_r1()),                // CMD8 MMC R1
        Ok(ok_r1()),                // CMD6 BUS_WIDTH=8
        Ok(r1_tran_ready()),        // CMD13
        // try_hs200 attempts HS_TIMING=2 + tuning, then fails:
        Ok(ok_r1()),         // CMD6 HS_TIMING=2
        Ok(r1_tran_ready()), // CMD13 (post-switch)
        // tuning fails — driver falls through to HS @ 52 MHz:
        Ok(ok_r1()),         // CMD6 HS_TIMING=1
        Ok(r1_tran_ready()), // CMD13 (post-switch)
    ];
    let mut host = MockHost::with_results(replies);
    host.next_read_payload = Some(ext_csd_blob_hs200());
    host.tuning_result = Some(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 21)));
    host.multi_step_hs200_rollback = true;
    let mut driver = SdMmcCard::new(host);
    let _info =
        poll_init_to_completion(&mut driver).expect("init succeeds even when HS200 tuning fails");

    // We *did* attempt HS200 — voltage switched to 1.8 V, tuning called,
    // then the rollback reverted voltage to 3.3 V so the controller's
    // 1.8 V sampling reference doesn't bleed into the HS@52 retry.
    let voltage_switches: Vec<SignalVoltage> = driver
        .host()
        .events
        .iter()
        .filter_map(|event| match event {
            MockEvent::Voltage(v) => Some(*v),
            _ => None,
        })
        .collect();
    // Voltage events look like: [V330 (init defensive reset), V180
    // (HS200 attempt), V330 (HS200 rollback)]. The leading V330 is the
    // abort_init cleanup that `submit_init` runs upfront to guarantee a
    // known controller state.
    assert_eq!(
        voltage_switches,
        std::vec![
            SignalVoltage::V330,
            SignalVoltage::V180,
            SignalVoltage::V330
        ]
    );
    assert_eq!(
        driver.host().last_tuning,
        Some((21, crate::cmd::MMC_TUNING_BLOCK_SIZE_8BIT as u16))
    );
    // But ended up at HighSpeed, not Hs200.
    assert_eq!(driver.host().last_clock, Some(ClockSpeed::HighSpeed));

    // Two CMD6 SWITCHes for HS_TIMING: first =2 (HS200, failed),
    // then =1 (HS @ 52 MHz, succeeded).
    let hs_timing_writes: Vec<u8> = driver
        .host()
        .commands
        .iter()
        .filter(|c| c.index == 6 && ((c.argument >> 16) & 0xFF) as u8 == 185)
        .map(|c| ((c.argument >> 8) & 0xFF) as u8)
        .collect();
    assert_eq!(hs_timing_writes, std::vec![0x02, 0x01]);
    assert!(
        driver.host().aborted_bus_ops.is_empty(),
        "HS200 fallback must drive every rollback bus operation to completion"
    );
}

#[test]
fn mmc_init_skips_hs200_when_host_refuses_voltage_switch() {
    // Card advertises HS200 @ 1.8 V, but the host has no way to drive
    // the IO rail at 1.8 V and refuses `switch_voltage(V180)` with
    // `UnsupportedCommand` (the rk3568 SDHCI default until a regulator
    // hook is wired up). The driver must NOT issue the HS_TIMING=2
    // SWITCH or call `execute_tuning`; leaving the controller's 1.8 V
    // signaling bit set while the bus is still on the 3.3 V rail
    // corrupts subsequent transfers. The driver should fall straight
    // through to HS @ 52 MHz.
    let replies = std::vec![
        Ok(ok_r1()),                // CMD0
        cmd8_timeout(),             // CMD8
        Ok(ok_r1()),                // CMD55
        acmd41_timeout(),           // ACMD41
        Ok(ocr_ready_mmc_sector()), // CMD1
        Ok(cid_response()),         // CMD2
        Ok(ok_r1()),                // CMD3
        Ok(csd_v2_response()),      // CMD9
        Ok(ok_r1()),                // CMD7
        Ok(ok_r1()),                // CMD8 MMC R1
        Ok(ok_r1()),                // CMD6 BUS_WIDTH=8
        Ok(r1_tran_ready()),        // CMD13
        // HS200 skipped — only HS_TIMING=1 + CMD13:
        Ok(ok_r1()),         // CMD6 HS_TIMING=1
        Ok(r1_tran_ready()), // CMD13
    ];
    let mut host = MockHost::with_results(replies);
    host.next_read_payload = Some(ext_csd_blob_hs200());
    host.voltage_switch_result = Some(Error::UnsupportedCommand);

    let mut driver = SdMmcCard::new(host);
    let _info = poll_init_to_completion(&mut driver)
        .expect("init succeeds when host refuses V180 voltage switch");

    // V180 was asked for once (and refused); no V330 rollback is needed
    // because submission transferred no request ownership and no HS200
    // command was issued. The only V330 event is the normal initialization
    // baseline.
    let voltage_switches: Vec<SignalVoltage> = driver
        .host()
        .events
        .iter()
        .filter_map(|event| match event {
            MockEvent::Voltage(voltage) => Some(*voltage),
            _ => None,
        })
        .collect();
    assert_eq!(
        voltage_switches,
        std::vec![SignalVoltage::V330, SignalVoltage::V180]
    );
    // Verify HS200 was NOT entered: no HS_TIMING=2, no tuning, final clock
    // is HighSpeed.
    assert_eq!(driver.host().last_tuning, None);
    assert_eq!(driver.host().last_clock, Some(ClockSpeed::HighSpeed));
    let hs_timing_writes: Vec<u8> = driver
        .host()
        .commands
        .iter()
        .filter(|c| c.index == 6 && ((c.argument >> 16) & 0xFF) as u8 == 185)
        .map(|c| ((c.argument >> 8) & 0xFF) as u8)
        .collect();
    assert_eq!(hs_timing_writes, std::vec![0x01]);
}
