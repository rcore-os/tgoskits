//! SDIO (Secure Digital Input Output) mode transport layer
//!
//! SDIO mode uses a dedicated host controller with 1-bit or 4-bit data bus.
//! Implement [`SdioHost`] for your platform's SDIO peripheral; the host
//! implementation controls command/data progress.

pub mod card;
pub mod host;
pub mod host2;
pub mod init;

use core::num::NonZeroU16;

pub use card::{
    CardInfo, CardKind, ExtCsdRequest, SdioCommandRequest, SdioDataRequest, SdioSdmmc,
    SdioStatusRequest, SwitchFunctionRequest,
};
pub use host::{
    BusWidth, ClockSpeed, HostEvent, HostEventKind, HostEventSource, ReadyBusRequest,
    SDMMC_BLOCK_QUEUE_ID, SdioBusOp, SdioHost, SdioIrqHandle, SdioIrqHost, SignalVoltage,
    block_queue_ready_from_host_event, poll_ready_bus_op, submit_ready_bus_op,
};
#[cfg(test)]
use host2::SDIO_HOST2_COMPAT_POLL_LIMIT;
pub use host2::{SdioHost2Adapter, SdioHost2BusRequest, SdioHost2DataRequest, SdioHost2Irq};
pub use init::{CardInitPreference, MmcSwitchRequest, SdioInitRequest, SdioInitScratch};
#[cfg(test)]
use init::{MmcSwitchTiming, SdioInitState, SdioInitTiming, sd_acmd6_arg};

pub use crate::cmd::DataDirection;
use crate::error::Error;

pub(super) fn nonzero_block_size(block_size: u32) -> Result<NonZeroU16, Error> {
    u16::try_from(block_size)
        .ok()
        .and_then(NonZeroU16::new)
        .ok_or(Error::InvalidArgument)
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;
    use crate::{
        CommandResponsePoll, DataCommandPoll, OperationPoll,
        cmd::Command,
        error::{ErrorContext, Phase},
        response::{
            CardState, IfCondResponse, OcrResponse, R1Response, RcaResponse, Response, ResponseType,
        },
    };

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum MockEvent {
        Command(Command),
        Clock(ClockSpeed),
        Voltage(SignalVoltage),
    }

    /// Mock host that replays canned responses in order. Used to verify the
    /// init sequence and that the driver tracks RCA on its own.
    struct MockHost {
        replies: Vec<Result<Response, Error>>,
        commands: Vec<Command>,
        events: Vec<MockEvent>,
        bus_width: Option<BusWidth>,
        data_requests: Vec<(DataDirection, u32, u32)>,
        next_read_payload: Option<Vec<u8>>,
        read_payloads: Vec<Vec<u8>>,
        writes: Vec<Vec<u8>>,
        /// When set, `set_bus_width(Bit8)` returns `UnsupportedCommand`
        /// to mimic a host (e.g. the SDHCI MVP backend) that hasn't
        /// wired up 8-bit operation yet.
        reject_bit8: bool,
        /// Last clock the protocol layer asked for. Lets HS200 tests
        /// confirm the host was driven up to 200 MHz.
        last_clock: Option<ClockSpeed>,
        /// Last voltage the protocol layer asked for. `None` means the
        /// driver never called `switch_voltage`.
        last_voltage: Option<SignalVoltage>,
        /// When `Some`, `switch_voltage` returns this error instead of
        /// succeeding. `Some(UnsupportedCommand)` exercises the
        /// "host has eMMC hard-wired at 1.8 V" path.
        voltage_switch_result: Option<Error>,
        /// When `Some`, `execute_tuning` returns this error. Lets the
        /// HS200-fallback test simulate a controller that can't tune.
        tuning_result: Option<Error>,
        /// Records the most recent `execute_tuning` call.
        last_tuning: Option<(u8, u16)>,
        pending_polls: usize,
        /// Optional monotonic clock value returned from
        /// [`SdioHost::now_ms`]. Tests advance this directly to verify the
        /// wall-clock timeout path; `None` keeps the legacy poll-counter
        /// behavior used by every pre-existing test.
        now_ms: Option<u64>,
    }

    struct MockDataRequest<'a> {
        response: Option<Response>,
        _marker: core::marker::PhantomData<&'a ()>,
    }

    impl MockHost {
        fn new(replies: Vec<Response>) -> Self {
            Self {
                replies: replies.into_iter().map(Ok).collect(),
                commands: Vec::new(),
                events: Vec::new(),
                bus_width: None,
                data_requests: Vec::new(),
                next_read_payload: None,
                read_payloads: Vec::new(),
                writes: Vec::new(),
                reject_bit8: false,
                last_clock: None,
                last_voltage: None,
                voltage_switch_result: None,
                tuning_result: None,
                last_tuning: None,
                pending_polls: 0,
                now_ms: None,
            }
        }

        /// Build a host where any response slot can be a synthesized
        /// error (e.g. a CMD8 timeout to simulate an eMMC card).
        fn with_results(replies: Vec<Result<Response, Error>>) -> Self {
            Self {
                replies,
                commands: Vec::new(),
                events: Vec::new(),
                bus_width: None,
                data_requests: Vec::new(),
                next_read_payload: None,
                read_payloads: Vec::new(),
                writes: Vec::new(),
                reject_bit8: false,
                last_clock: None,
                last_voltage: None,
                voltage_switch_result: None,
                tuning_result: None,
                last_tuning: None,
                pending_polls: 0,
                now_ms: None,
            }
        }
    }

    impl SdioHost for MockHost {
        type Event = ();
        type DataRequest<'a> = MockDataRequest<'a>;
        type BusRequest = ReadyBusRequest;

        fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
            self.commands.push(*cmd);
            self.events.push(MockEvent::Command(*cmd));
            Ok(())
        }

        fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error> {
            if self.pending_polls > 0 {
                self.pending_polls -= 1;
                return Ok(CommandResponsePoll::Pending);
            }
            if self.replies.is_empty() {
                return Err(Error::Timeout(ErrorContext::default()));
            }
            self.replies.remove(0).map(CommandResponsePoll::Complete)
        }

        fn submit_read_data<'a>(
            &mut self,
            cmd: &Command,
            buf: &'a mut [u8],
            block_size: u32,
            block_count: u32,
        ) -> Result<Self::DataRequest<'a>, Error> {
            self.data_requests
                .push((DataDirection::Read, block_size, block_count));
            self.submit_command(cmd)?;
            let CommandResponsePoll::Complete(response) = self.poll_command_response()? else {
                return Err(Error::Timeout(ErrorContext::default()));
            };
            let payload = if self.read_payloads.is_empty() {
                self.next_read_payload.take()
            } else {
                Some(self.read_payloads.remove(0))
            };
            match payload {
                Some(data) if data.len() == buf.len() => {
                    buf.copy_from_slice(&data);
                    Ok(MockDataRequest {
                        response: Some(response),
                        _marker: core::marker::PhantomData,
                    })
                }
                _ => Err(Error::UnsupportedCommand),
            }
        }

        fn submit_write_data<'a>(
            &mut self,
            cmd: &Command,
            buf: &'a [u8],
            block_size: u32,
            block_count: u32,
        ) -> Result<Self::DataRequest<'a>, Error> {
            self.data_requests
                .push((DataDirection::Write, block_size, block_count));
            self.submit_command(cmd)?;
            let CommandResponsePoll::Complete(response) = self.poll_command_response()? else {
                return Err(Error::Timeout(ErrorContext::default()));
            };
            self.writes.push(buf.to_vec());
            Ok(MockDataRequest {
                response: Some(response),
                _marker: core::marker::PhantomData,
            })
        }

        fn poll_data_request<'a>(
            &mut self,
            request: &mut Self::DataRequest<'a>,
        ) -> Result<DataCommandPoll, Error> {
            request
                .response
                .take()
                .map(DataCommandPoll::Complete)
                .ok_or(Error::InvalidArgument)
        }

        fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
            if self.reject_bit8 && matches!(width, BusWidth::Bit8) {
                return Err(Error::UnsupportedCommand);
            }
            self.bus_width = Some(width);
            Ok(())
        }

        fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
            self.last_clock = Some(speed);
            self.events.push(MockEvent::Clock(speed));
            Ok(())
        }

        fn switch_voltage(&mut self, v: SignalVoltage) -> Result<(), Error> {
            self.last_voltage = Some(v);
            self.events.push(MockEvent::Voltage(v));
            if let Some(e) = self.voltage_switch_result {
                return Err(e);
            }
            Ok(())
        }

        fn execute_tuning(&mut self, cmd_index: u8, block_size: NonZeroU16) -> Result<(), Error> {
            self.last_tuning = Some((cmd_index, block_size.get()));
            if let Some(e) = self.tuning_result {
                return Err(e);
            }
            Ok(())
        }

        fn submit_bus_op(&mut self, op: SdioBusOp) -> Result<Self::BusRequest, Error> {
            submit_ready_bus_op(self, op)
        }

        fn poll_bus_op(
            &mut self,
            request: &mut Self::BusRequest,
        ) -> Result<OperationPoll<()>, Error> {
            poll_ready_bus_op(request)
        }

        fn now_ms(&self) -> Option<u64> {
            self.now_ms
        }
    }

    #[test]
    fn sdio_host_irq_methods_default_to_noop() {
        let mut host = MockHost::new(Vec::new());

        assert_eq!(host.enable_completion_irq(), Ok(()));
        assert_eq!(host.disable_completion_irq(), Ok(()));
    }

    #[test]
    fn unit_irq_event_reports_no_runtime_action() {
        let event = ();

        assert_eq!(event.kind(), HostEventKind::None);
        assert_eq!(event.source(), HostEventSource::Controller);
        assert_eq!(event.queue_id(), None);
    }

    fn ok_r1() -> Response {
        Response::R1(R1Response::from_native_raw(0).unwrap())
    }

    fn rca_response(rca: u16) -> Response {
        Response::R6(RcaResponse::from_raw((rca as u32) << 16))
    }

    fn ocr_ready_sdhc() -> Response {
        // bit 31 = power-up done, bit 30 = CCS (high capacity)
        Response::R3(OcrResponse::from_raw(0xC0FF_8000))
    }

    fn ocr_ready_sdhc_s18a() -> Response {
        // bit 31 = power-up done, bit 30 = CCS, bit 24 = S18A
        Response::R3(OcrResponse::from_raw(0xC1FF_8000))
    }

    fn csd_v2_response() -> Response {
        let mut raw = [0u8; 16];
        raw[0] = 0x40;
        raw[7] = 0x00;
        raw[8] = 0x0F;
        raw[9] = 0x0F;
        Response::R2(raw)
    }

    fn cid_response() -> Response {
        let mut raw = [0u8; 16];
        raw[0] = 0x03;
        raw[1] = b'S';
        raw[2] = b'D';
        raw[3] = b'A';
        raw[4] = b'B';
        raw[5] = b'C';
        raw[6] = b'1';
        raw[7] = b'2';
        Response::R2(raw)
    }

    fn sd_init_replies() -> Vec<Result<Response, Error>> {
        sd_init_replies_with_ocr(ocr_ready_sdhc())
    }

    fn disable_speed_selection(driver: &mut SdioSdmmc<MockHost>) {
        driver.set_sd_speed_selection_enabled(false);
    }

    fn sd_init_replies_with_ocr(ocr: Response) -> Vec<Result<Response, Error>> {
        std::vec![
            Ok(ok_r1()),                                             // CMD0
            Ok(Response::R7(IfCondResponse::from_raw(0x0000_01AA))), // CMD8
            Ok(ok_r1()),                                             // CMD55 (ACMD41 prologue)
            Ok(ocr),                                                 // ACMD41
            Ok(cid_response()),                                      // CMD2
            Ok(rca_response(0x1234)),                                // CMD3
            Ok(csd_v2_response()),                                   // CMD9
            Ok(ok_r1()),                                             // CMD7 (select)
            Ok(ok_r1()),                                             // CMD55 (ACMD6 prologue)
            Ok(ok_r1()),                                             // ACMD6
        ]
    }

    fn switch_status_payload(function: u8, supported: u8) -> Vec<u8> {
        let mut status = std::vec![0u8; 64];
        status[13] = supported;
        status[16] = function & 0x0f;
        status
    }

    fn poll_init_to_completion<H: SdioHost>(driver: &mut SdioSdmmc<H>) -> Result<CardInfo, Error> {
        poll_init_to_completion_with_preference(driver, CardInitPreference::SdFirst)
    }

    fn poll_init_to_completion_with_preference<H: SdioHost>(
        driver: &mut SdioSdmmc<H>,
        preference: CardInitPreference,
    ) -> Result<CardInfo, Error> {
        let mut scratch = SdioInitScratch::new();
        let mut request = driver.submit_init_with_preference(preference, &mut scratch)?;
        loop {
            match driver.poll_init_request(&mut request)? {
                OperationPoll::Pending => {}
                OperationPoll::Complete(info) => return Ok(info),
            }
        }
    }

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

        let err = poll_init_to_completion(&mut driver)
            .expect_err("init must propagate the injected failure");
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

        assert_eq!(driver.host.last_voltage, Some(SignalVoltage::V180));
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::Sdr104));
        assert_eq!(
            driver.host.last_tuning,
            Some((19, crate::cmd::SD_TUNING_BLOCK_SIZE as u16))
        );
        assert!(
            driver.host.commands.iter().any(|c| c.index == 11),
            "CMD11 issued before host voltage switch"
        );
        assert!(
            driver
                .host
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
                .host
                .events
                .iter()
                .any(|e| matches!(e, MockEvent::Voltage(SignalVoltage::V180))),
            "legacy-HighSpeed init must never ask the host for 1.8 V"
        );
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::HighSpeed));
        assert_eq!(driver.host.last_tuning, None);
        assert!(
            !driver.host.commands.iter().any(|c| c.index == 11),
            "CMD11 voltage switch must not be issued in legacy HighSpeed-only mode"
        );
        assert!(
            driver
                .host
                .commands
                .iter()
                .any(|c| c.index == 6 && c.argument == 0x80FF_FFF1),
            "CMD6 switched group 1 to HighSpeed"
        );
        assert!(
            !driver
                .host
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
        poll_init_to_completion(&mut driver)
            .expect("SD init falls back when UHS voltage switch fails");

        assert_eq!(driver.host.last_voltage, Some(SignalVoltage::V180));
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::HighSpeed));
        assert_eq!(driver.host.last_tuning, None);
        assert!(
            driver
                .host
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
        let mut scratch = SdioInitScratch::new();
        let mut request = driver.submit_init(&mut scratch).unwrap();

        for _ in 0..4 {
            assert!(matches!(
                driver.poll_init_request(&mut request).unwrap(),
                OperationPoll::Pending
            ));
        }
        assert!(matches!(
            driver.poll_init_request(&mut request),
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

        poll_init_to_completion(&mut driver)
            .expect("SD init succeeds without CMD6 speed switching");

        assert_eq!(driver.host.bus_width, Some(BusWidth::Bit4));
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::Default));
        assert!(
            driver
                .host
                .commands
                .iter()
                .filter(|c| c.index == 6)
                .all(|c| c.argument == 2),
            "only ACMD6 bus-width switch is issued; no CMD6 SWITCH_FUNC"
        );
        assert!(
            !driver
                .host
                .events
                .iter()
                .any(|e| matches!(e, MockEvent::Voltage(SignalVoltage::V180))),
            "speed-selection-disabled init must never ask the host for 1.8 V"
        );
        assert_eq!(driver.host.last_tuning, None);
    }

    fn ocr_ready_mmc_sector() -> Response {
        // bit 31 = power-up done, bit 30 = sector mode (high capacity)
        Response::R3(OcrResponse::from_raw(0xC0FF_8000))
    }

    fn cmd8_timeout() -> Result<Response, Error> {
        Err(Error::Timeout(ErrorContext::for_cmd(Phase::CommandSend, 8)))
    }

    fn acmd41_timeout() -> Result<Response, Error> {
        Err(Error::Timeout(ErrorContext::for_cmd(
            Phase::CommandSend,
            41,
        )))
    }

    /// CMD13 R1 with `READY_FOR_DATA` set and the card in `tran` state.
    /// What `mmc_switch` polls for after a CMD6 SWITCH.
    fn r1_tran_ready() -> Response {
        // bit 8 = READY_FOR_DATA, bits 12..9 = 4 (Transfer)
        Response::R1(R1Response::from_native_raw((1 << 8) | (4 << 9)).unwrap())
    }

    /// Build an EXT_CSD payload that advertises 8-bit, HS @ 52 MHz, and
    /// a sector count.
    fn ext_csd_blob() -> Vec<u8> {
        use crate::cmd::ext_csd as e;
        let mut buf = std::vec![0u8; 512];
        // SEC_COUNT = 0x0080_0000 (4 GiB) little-endian
        buf[e::SEC_COUNT] = 0x00;
        buf[e::SEC_COUNT + 1] = 0x00;
        buf[e::SEC_COUNT + 2] = 0x80;
        buf[e::SEC_COUNT + 3] = 0x00;
        // DEVICE_TYPE = HS_26 | HS_52
        buf[e::DEVICE_TYPE] = e::device_type::HS_26 | e::device_type::HS_52;
        // Currently selected: 1-bit, compat (matches reset state)
        buf[e::BUS_WIDTH] = 0;
        buf[e::HS_TIMING] = 0;
        buf
    }

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
        let mut driver = SdioSdmmc::new(host);
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

        let cmds = &driver.host.commands;
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
        assert_eq!(driver.host.bus_width, Some(BusWidth::Bit8));
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
        let mut driver = SdioSdmmc::new(host);
        let _info =
            poll_init_to_completion(&mut driver).expect("eMMC init succeeds with 4-bit fallback");

        assert_eq!(driver.host.bus_width, Some(BusWidth::Bit4));
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
        let mut driver = SdioSdmmc::new(host);
        disable_speed_selection(&mut driver);
        let info = poll_init_to_completion(&mut driver).expect("SD v1 init succeeds");

        assert_eq!(info.kind, CardKind::Sd, "ACMD41 success → SD, not MMC");
        assert!(!info.sd_v2);
        assert!(!info.high_capacity);
        assert_eq!(info.rca, 0x4321);
        assert_eq!(driver.host.bus_width, Some(BusWidth::Bit4));
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
        let mut driver = SdioSdmmc::new(host);
        let _info = poll_init_to_completion(&mut driver).expect("HS200 init succeeds");

        // HS_TIMING write should carry value 0x02, not 0x01.
        let cmd6s: Vec<&Command> = driver
            .host
            .commands
            .iter()
            .filter(|c| c.index == 6)
            .collect();
        // Two CMD6: BUS_WIDTH(=2) and HS_TIMING(=2)
        assert_eq!(cmd6s.len(), 2);
        let hs_timing_arg = (0b11u32 << 24) | ((185u32) << 16) | (0x02u32 << 8);
        assert_eq!(cmd6s[1].argument, hs_timing_arg, "HS_TIMING=2 (HS200)");

        // Host hooks were exercised.
        assert_eq!(driver.host.last_voltage, Some(SignalVoltage::V180));
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::Hs200));
        assert_eq!(
            driver.host.last_tuning,
            Some((21, crate::cmd::MMC_TUNING_BLOCK_SIZE_8BIT as u16))
        );

        let hs200_clock_pos = driver
            .host
            .events
            .iter()
            .position(|event| matches!(event, MockEvent::Clock(ClockSpeed::Hs200)))
            .expect("host clock is raised to HS200");
        let hs200_switch_pos = driver
            .host
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
        let mut driver = SdioSdmmc::new(host);
        let _info = poll_init_to_completion(&mut driver)
            .expect("init succeeds even when HS200 tuning fails");

        // We *did* attempt HS200 — voltage switched to 1.8 V, tuning called,
        // then the rollback reverted voltage to 3.3 V so the controller's
        // 1.8 V sampling reference doesn't bleed into the HS@52 retry.
        let voltage_switches: Vec<SignalVoltage> = driver
            .host
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
            driver.host.last_tuning,
            Some((21, crate::cmd::MMC_TUNING_BLOCK_SIZE_8BIT as u16))
        );
        // But ended up at HighSpeed, not Hs200.
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::HighSpeed));

        // Two CMD6 SWITCHes for HS_TIMING: first =2 (HS200, failed),
        // then =1 (HS @ 52 MHz, succeeded).
        let hs_timing_writes: Vec<u8> = driver
            .host
            .commands
            .iter()
            .filter(|c| c.index == 6 && ((c.argument >> 16) & 0xFF) as u8 == 185)
            .map(|c| ((c.argument >> 8) & 0xFF) as u8)
            .collect();
        assert_eq!(hs_timing_writes, std::vec![0x02, 0x01]);
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

        let mut driver = SdioSdmmc::new(host);
        let _info = poll_init_to_completion(&mut driver)
            .expect("init succeeds when host refuses V180 voltage switch");

        // V180 was asked for once (and refused); no V330 rollback is needed
        // because no HS200 commands were issued, but the protocol may emit
        // it defensively. Verify HS200 was NOT entered: no HS_TIMING=2,
        // no tuning, final clock is HighSpeed.
        assert_eq!(driver.host.last_tuning, None);
        assert_eq!(driver.host.last_clock, Some(ClockSpeed::HighSpeed));
        let hs_timing_writes: Vec<u8> = driver
            .host
            .commands
            .iter()
            .filter(|c| c.index == 6 && ((c.argument >> 16) & 0xFF) as u8 == 185)
            .map(|c| ((c.argument >> 8) & 0xFF) as u8)
            .collect();
        assert_eq!(hs_timing_writes, std::vec![0x01]);
    }

    #[test]
    fn set_bus_width_bit8_is_unsupported_via_acmd6() {
        assert_eq!(sd_acmd6_arg(BusWidth::Bit8), Err(Error::UnsupportedCommand));
    }

    #[test]
    fn submit_read_blocks_into_leaves_multi_block_stop_to_host_request() {
        let mut host = MockHost::new(std::vec![ok_r1()]);
        let expected: Vec<u8> = (0..1024).map(|i| (i % 251) as u8).collect();
        host.next_read_payload = Some(expected.clone());

        let mut driver = SdioSdmmc::new(host);
        driver.high_capacity = true;
        let mut buf = [0u8; 1024];

        let mut request = driver.submit_read_blocks_into(7, &mut buf).unwrap();
        assert!(matches!(
            driver.poll_data_request(&mut request).unwrap(),
            DataCommandPoll::Complete(_)
        ));

        assert_eq!(&buf[..], &expected[..]);
        assert_eq!(
            driver.host.data_requests,
            std::vec![(DataDirection::Read, 512, 2)]
        );
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|c| c.index)
                .collect::<Vec<_>>(),
            std::vec![18]
        );
        assert_eq!(driver.host.commands[0].argument, 7);
    }

    #[test]
    fn submit_write_blocks_from_leaves_multi_block_stop_to_host_request() {
        let host = MockHost::new(std::vec![ok_r1()]);
        let mut driver = SdioSdmmc::new(host);
        driver.high_capacity = true;
        let buf = [0x5au8; 1024];

        let mut request = driver.submit_write_blocks_from(11, &buf).unwrap();
        assert!(matches!(
            driver.poll_data_request(&mut request).unwrap(),
            DataCommandPoll::Complete(_)
        ));

        assert_eq!(
            driver.host.data_requests,
            std::vec![(DataDirection::Write, 512, 2)]
        );
        assert_eq!(
            driver
                .host
                .commands
                .iter()
                .map(|c| c.index)
                .collect::<Vec<_>>(),
            std::vec![25]
        );
        assert_eq!(driver.host.commands[0].argument, 11);
        assert_eq!(driver.host.writes, std::vec![buf.to_vec()]);
    }

    #[test]
    fn submit_block_io_rejects_misaligned_buffers() {
        let host = MockHost::new(std::vec![]);
        let mut driver = SdioSdmmc::new(host);
        let mut read_buf = [0u8; 513];
        let write_buf = [0u8; 513];

        assert_eq!(
            driver.submit_read_blocks_into(0, &mut read_buf).map(|_| ()),
            Err(Error::Misaligned)
        );
        assert_eq!(
            driver.submit_write_blocks_from(0, &write_buf).map(|_| ()),
            Err(Error::Misaligned)
        );
        assert!(driver.host.commands.is_empty());
    }

    struct MockIrqHandle {
        event: IrqTestEvent,
    }

    impl SdioIrqHandle for MockIrqHandle {
        type Event = IrqTestEvent;

        fn handle_irq(&mut self) -> Self::Event {
            self.event
        }
    }

    #[derive(Clone, Copy, Default)]
    struct IrqTestEvent(HostEventKind);

    impl HostEvent for IrqTestEvent {
        fn kind(&self) -> HostEventKind {
            self.0
        }
    }

    #[test]
    fn host_irq_events_map_to_single_sdmmc_block_queue() {
        assert_eq!(
            block_queue_ready_from_host_event(&IrqTestEvent(HostEventKind::None)),
            None
        );
        for kind in [
            HostEventKind::CommandComplete,
            HostEventKind::TransferComplete,
            HostEventKind::ReceiveReady,
            HostEventKind::TransmitReady,
            HostEventKind::Error,
            HostEventKind::Other,
        ] {
            assert_eq!(
                block_queue_ready_from_host_event(&IrqTestEvent(kind)),
                Some(SDMMC_BLOCK_QUEUE_ID)
            );
        }
    }

    #[test]
    fn irq_handle_is_move_only_and_handles_with_mutable_endpoint() {
        let mut handle = MockIrqHandle {
            event: IrqTestEvent(HostEventKind::TransferComplete),
        };

        assert_eq!(handle.handle_irq().kind(), HostEventKind::TransferComplete);
    }

    struct Host2Mock {
        transactions: Vec<(
            Command,
            Option<(sdio_host2::DataDirection, usize, u32, u32)>,
        )>,
        bus_ops: Vec<sdio_host2::BusOp>,
        response: sdio_host2::RawResponse,
        transaction_error: Option<sdio_host2::Error>,
        bus_pending_polls: usize,
        bus_error: Option<sdio_host2::Error>,
        transaction_aborts: usize,
        bus_aborts: usize,
        completion_irq_enabled: bool,
    }

    struct Host2TransactionRequest {
        response: sdio_host2::RawResponse,
        pending_polls: usize,
        done: bool,
    }

    struct Host2BusRequest {
        pending_polls: usize,
        done: bool,
    }

    impl sdio_host2::SdioHost for Host2Mock {
        type TransactionRequest<'a>
            = Host2TransactionRequest
        where
            Self: 'a;
        type BusRequest = Host2BusRequest;

        unsafe fn submit_transaction<'a>(
            &mut self,
            transaction: sdio_host2::Transaction<'a>,
        ) -> Result<Self::TransactionRequest<'a>, sdio_host2::Error>
        where
            Self: 'a,
        {
            let data = transaction.data.as_ref().map(|phase| {
                (
                    phase.direction,
                    phase.buffer.len(),
                    u32::from(phase.block_size.get()),
                    phase.block_count.get(),
                )
            });
            self.transactions.push((transaction.command, data));
            Ok(Host2TransactionRequest {
                response: self.response,
                pending_polls: 0,
                done: false,
            })
        }

        fn poll_transaction<'a>(
            &mut self,
            request: &mut Self::TransactionRequest<'a>,
        ) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
        where
            Self: 'a,
        {
            if request.done {
                return Err(sdio_host2::PollRequestError::AlreadyCompleted);
            }
            if request.pending_polls > 0 {
                request.pending_polls -= 1;
                return Ok(sdio_host2::RequestPoll::Pending);
            }
            if let Some(err) = self.transaction_error.take() {
                request.done = true;
                return Ok(sdio_host2::RequestPoll::Ready(Err(err)));
            }
            request.done = true;
            Ok(sdio_host2::RequestPoll::Ready(Ok(request.response)))
        }

        fn abort_transaction<'a>(
            &mut self,
            request: &mut Self::TransactionRequest<'a>,
        ) -> Result<(), sdio_host2::Error>
        where
            Self: 'a,
        {
            if !request.done {
                self.transaction_aborts += 1;
                request.done = true;
            }
            Ok(())
        }

        unsafe fn submit_bus_op(
            &mut self,
            op: sdio_host2::BusOp,
        ) -> Result<Self::BusRequest, sdio_host2::Error> {
            self.bus_ops.push(op);
            Ok(Host2BusRequest {
                pending_polls: self.bus_pending_polls,
                done: false,
            })
        }

        fn poll_bus_op(
            &mut self,
            request: &mut Self::BusRequest,
        ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError> {
            if request.done {
                return Err(sdio_host2::PollRequestError::AlreadyCompleted);
            }
            if request.pending_polls > 0 {
                request.pending_polls -= 1;
                return Ok(sdio_host2::RequestPoll::Pending);
            }
            if let Some(err) = self.bus_error.take() {
                request.done = true;
                return Ok(sdio_host2::RequestPoll::Ready(Err(err)));
            }
            request.done = true;
            Ok(sdio_host2::RequestPoll::Ready(Ok(())))
        }

        fn abort_bus_op(
            &mut self,
            request: &mut Self::BusRequest,
        ) -> Result<(), sdio_host2::Error> {
            if !request.done {
                self.bus_aborts += 1;
                request.done = true;
            }
            Ok(())
        }
    }

    impl SdioHost2Irq for Host2Mock {
        type Event = ();
        type IrqHandle = Host2MockIrq;

        fn completion_irq_enabled(&self) -> bool {
            self.completion_irq_enabled
        }

        fn enable_completion_irq(&mut self) -> Result<(), Error> {
            self.completion_irq_enabled = true;
            Ok(())
        }

        fn disable_completion_irq(&mut self) -> Result<(), Error> {
            self.completion_irq_enabled = false;
            Ok(())
        }

        fn irq_handle(&mut self) -> Self::IrqHandle {
            Host2MockIrq
        }
    }

    struct Host2MockIrq;

    impl SdioIrqHandle for Host2MockIrq {
        type Event = ();

        fn handle_irq(&mut self) -> Self::Event {}
    }

    impl Host2Mock {
        fn new(response: sdio_host2::RawResponse) -> Self {
            Self {
                transactions: Vec::new(),
                bus_ops: Vec::new(),
                response,
                transaction_error: None,
                bus_pending_polls: 0,
                bus_error: None,
                transaction_aborts: 0,
                bus_aborts: 0,
                completion_irq_enabled: false,
            }
        }
    }

    #[test]
    fn host2_adapter_reports_forwarded_completion_irq_state() {
        let host = Host2Mock::new(sdio_host2::RawResponse::empty());
        let mut adapter = SdioHost2Adapter::new(host);

        assert!(!adapter.completion_irq_enabled());
        adapter.enable_completion_irq().unwrap();
        assert!(adapter.completion_irq_enabled());
        adapter.disable_completion_irq().unwrap();
        assert!(!adapter.completion_irq_enabled());
    }

    #[test]
    fn host2_adapter_submits_read_as_physical_transaction() {
        let host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
        let mut driver = SdioSdmmc::new_host2(host);
        driver.high_capacity = true;
        let mut buf = [0u8; 512];

        let mut request = driver.submit_read_blocks_into(9, &mut buf).unwrap();
        assert!(matches!(
            driver.poll_data_request(&mut request).unwrap(),
            DataCommandPoll::Complete(Response::R1(_))
        ));

        let transactions = driver.host().with_host(|host| host.transactions.clone());
        assert_eq!(transactions.len(), 1);
        assert_eq!(transactions[0].0.index, 17);
        assert_eq!(transactions[0].0.argument, 9);
        assert_eq!(
            transactions[0].1,
            Some((sdio_host2::DataDirection::Read, 512, 512, 1))
        );
    }

    #[test]
    fn host2_adapter_submits_bus_ops_for_clock_changes() {
        let host = Host2Mock::new(sdio_host2::RawResponse::empty());
        let mut driver = SdioSdmmc::new_host2(host);

        driver
            .host_mut()
            .set_clock(ClockSpeed::HighSpeed)
            .expect("bus op completes");

        assert_eq!(
            driver.host().with_host(|host| host.bus_ops.clone()),
            std::vec![sdio_host2::BusOp::SetClock(ClockSpeed::HighSpeed)]
        );
    }

    #[test]
    fn host2_adapter_poll_error_releases_active_command() {
        let mut host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
        host.transaction_error = Some(sdio_host2::Error::Timeout);
        let mut adapter = SdioHost2Adapter::new(host);
        let cmd = Command::new(13, 0, ResponseType::R1);

        adapter.submit_command(&cmd).unwrap();
        assert!(matches!(
            adapter.poll_command_response(),
            Err(Error::Timeout(_))
        ));

        adapter.submit_command(&cmd).unwrap();
    }

    #[test]
    fn host2_sync_bus_wrapper_drains_pending_request() {
        let mut host = Host2Mock::new(sdio_host2::RawResponse::empty());
        host.bus_pending_polls = 3;
        let mut driver = SdioSdmmc::new_host2(host);

        driver
            .host_mut()
            .set_clock(ClockSpeed::HighSpeed)
            .expect("compat wrapper drains pending bus request");

        assert_eq!(
            driver.host().with_host(|host| host.bus_ops.clone()),
            std::vec![sdio_host2::BusOp::SetClock(ClockSpeed::HighSpeed)]
        );
    }

    #[test]
    fn host2_init_bus_op_pending_is_observed_without_spinning() {
        let mut host = Host2Mock::new(sdio_host2::RawResponse::empty());
        host.bus_pending_polls = 1;
        let mut driver = SdioSdmmc::new_host2(host);
        let mut scratch = SdioInitScratch::new();

        let mut request = driver.submit_init(&mut scratch).unwrap();
        assert!(driver.host().with_host(|host| host.bus_ops.is_empty()));

        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(
            driver.host().with_host(|host| host.bus_ops.clone()),
            std::vec![sdio_host2::BusOp::ResetAll]
        );
        assert!(driver.host().with_host(|host| host.transactions.is_empty()));

        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(driver.host().with_host(|host| host.bus_ops.len()), 1);
        assert!(driver.host().with_host(|host| host.transactions.is_empty()));

        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(driver.host().with_host(|host| host.bus_ops.len()), 1);
        assert!(driver.host().with_host(|host| host.transactions.is_empty()));

        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert_eq!(
            driver.host().with_host(|host| host.bus_ops.clone()),
            std::vec![sdio_host2::BusOp::ResetAll, sdio_host2::BusOp::PowerOn]
        );
        assert!(driver.host().with_host(|host| host.transactions.is_empty()));
    }

    #[test]
    fn host2_init_starts_with_physical_bus_ops_before_cmd0() {
        let host = Host2Mock::new(sdio_host2::RawResponse::empty());
        let mut driver = SdioSdmmc::new_host2(host);
        let mut scratch = SdioInitScratch::new();
        let mut request = driver.submit_init(&mut scratch).unwrap();

        for _ in 0..16 {
            assert!(matches!(
                driver.poll_init_request(&mut request).unwrap(),
                OperationPoll::Pending
            ));
            if driver
                .host()
                .with_host(|host| !host.transactions.is_empty())
            {
                break;
            }
        }

        assert_eq!(
            driver.host().with_host(|host| host.bus_ops.clone()),
            std::vec![
                sdio_host2::BusOp::ResetAll,
                sdio_host2::BusOp::PowerOn,
                sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V330),
                sdio_host2::BusOp::SetBusWidth(BusWidth::Bit1),
                sdio_host2::BusOp::SetClock(ClockSpeed::Identification),
            ]
        );
        let transactions = driver.host().with_host(|host| host.transactions.clone());
        assert_eq!(transactions.len(), 1);
        assert_eq!(transactions[0].0.index, 0);
        assert!(transactions[0].1.is_none());
    }

    #[test]
    fn host2_init_bus_op_error_releases_request_slot() {
        let mut host = Host2Mock::new(sdio_host2::RawResponse::empty());
        host.bus_error = Some(sdio_host2::Error::Timeout);
        let mut driver = SdioSdmmc::new_host2(host);
        let mut scratch = SdioInitScratch::new();
        let mut request = driver.submit_init(&mut scratch).unwrap();

        assert!(matches!(
            driver.poll_init_request(&mut request).unwrap(),
            OperationPoll::Pending
        ));
        assert!(matches!(
            driver.poll_init_request(&mut request),
            Err(Error::Timeout(_))
        ));
        assert!(request.bus_request.is_none());
    }

    #[test]
    fn host2_adapter_drop_aborts_pending_data_request() {
        let host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
        let mut adapter = SdioHost2Adapter::new(host);
        let cmd = Command::new(17, 0, ResponseType::R1);
        let mut buf = [0u8; 512];

        let request = adapter.submit_read_data(&cmd, &mut buf, 512, 1).unwrap();
        drop(request);

        assert_eq!(adapter.with_host(|host| host.transaction_aborts), 1);
    }

    #[test]
    fn host2_sync_bus_timeout_aborts_pending_bus_request() {
        let mut host = Host2Mock::new(sdio_host2::RawResponse::empty());
        host.bus_pending_polls = (SDIO_HOST2_COMPAT_POLL_LIMIT as usize) + 1;
        let mut adapter = SdioHost2Adapter::new(host);

        assert!(matches!(
            adapter.set_clock(ClockSpeed::HighSpeed),
            Err(Error::Timeout(_))
        ));

        assert_eq!(adapter.with_host(|host| host.bus_aborts), 1);
    }
}
