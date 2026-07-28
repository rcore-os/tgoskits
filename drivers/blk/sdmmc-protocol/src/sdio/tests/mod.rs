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

mod block_io_irq;
mod host2_adapter;
mod init_flow;
mod mmc_init;
mod sd_speed;

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

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error> {
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
