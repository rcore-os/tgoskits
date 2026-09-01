extern crate std;

use std::{
    alloc::{Layout, alloc_zeroed, dealloc},
    ptr::NonNull,
    sync::OnceLock,
    vec::Vec,
};

use super::*;
use crate::{
    DataCommandProgress, OperationProgress,
    cmd::Command,
    error::{ErrorContext, Phase},
    response::{
        CardState, IfCondResponse, OcrResponse, R1Response, RcaResponse, Response, ResponseType,
    },
};

mod block_io_irq;
mod init_flow;
mod io_card;
mod mmc_init;
#[cfg(feature = "rdif")]
mod rdif_lifecycle;
mod sd_speed;
mod transport_adapter;

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
    data_requests: Vec<(sdmmc_host::DataDirection, u32, u32)>,
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
    /// Make the HS200 rollback voltage/clock operations require more than
    /// one register-state advance. This catches synchronous wrappers that
    /// submit a multi-step bus operation, advance it once, then abort it.
    multi_step_hs200_rollback: bool,
    aborted_bus_ops: Vec<sdmmc_host::BusOp>,
    pending_polls: usize,
    complete_after_irq_register_retry: bool,
    completion_irq_enabled: bool,
    /// Optional monotonic clock value returned from
    /// [`sdmmc_host::SdMmcHost::now_ms`]. Tests advance this directly to verify the
    /// wall-clock timeout path; `None` keeps the legacy poll-counter
    /// behavior used by every pre-existing test.
    now_ms: Option<u64>,
}

struct MockTransactionRequest {
    result: Option<Result<sdmmc_host::RawResponse, sdmmc_host::Error>>,
    dma: Option<dma_api::PreparedDma>,
    completed_dma: Option<dma_api::CompletedDma>,
    acknowledged_irq: bool,
}

struct MockBusRequest {
    op: sdmmc_host::BusOp,
    pending_advances: usize,
    done: bool,
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
            multi_step_hs200_rollback: false,
            aborted_bus_ops: Vec::new(),
            pending_polls: 0,
            complete_after_irq_register_retry: false,
            completion_irq_enabled: false,
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
            multi_step_hs200_rollback: false,
            aborted_bus_ops: Vec::new(),
            pending_polls: 0,
            complete_after_irq_register_retry: false,
            completion_irq_enabled: false,
            now_ms: None,
        }
    }
}

impl sdmmc_host::SdMmcHost for MockHost {
    type TransactionRequest<'a> = MockTransactionRequest;
    type BusRequest = MockBusRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        mut transaction: sdmmc_host::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdmmc_host::Error>
    where
        Self: 'a,
    {
        let command = transaction.command;
        self.commands.push(command);
        self.events.push(MockEvent::Command(command));
        let result = if self.replies.is_empty() {
            Err(sdmmc_host::Error::Timeout)
        } else {
            self.replies
                .remove(0)
                .map(|response| response.to_raw_response(command.response))
                .map_err(protocol_error_to_host)
        };
        let mut dma = None;
        if let Some(data) = transaction.data.take() {
            self.data_requests.push((
                data.direction,
                u32::from(data.block_size.get()),
                data.block_count.get(),
            ));
            match data.buffer {
                sdmmc_host::DataBuffer::Read(buffer) => {
                    let payload = if self.read_payloads.is_empty() {
                        self.next_read_payload.take()
                    } else {
                        Some(self.read_payloads.remove(0))
                    };
                    let Some(payload) = payload.filter(|payload| payload.len() == buffer.len())
                    else {
                        return Err(sdmmc_host::Error::Unsupported);
                    };
                    buffer.copy_from_slice(&payload);
                }
                sdmmc_host::DataBuffer::Write(buffer) => self.writes.push(buffer.to_vec()),
                sdmmc_host::DataBuffer::Dma(buffer) => {
                    if data.direction == sdmmc_host::DataDirection::Read {
                        let payload = if self.read_payloads.is_empty() {
                            self.next_read_payload.take()
                        } else {
                            Some(self.read_payloads.remove(0))
                        };
                        let Some(payload) =
                            payload.filter(|payload| payload.len() == buffer.len().get())
                        else {
                            return Err(sdmmc_host::Error::Unsupported);
                        };
                        unsafe {
                            buffer
                                .cpu_ptr()
                                .as_ptr()
                                .copy_from_nonoverlapping(payload.as_ptr(), payload.len());
                        }
                    }
                    dma = Some(buffer);
                }
            }
        }
        Ok(MockTransactionRequest {
            result: Some(result),
            dma,
            completed_dma: None,
            acknowledged_irq: false,
        })
    }

    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: sdmmc_host::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdmmc_host::SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        if let Some(data) = transaction.data.as_ref()
            && data.direction == sdmmc_host::DataDirection::Read
        {
            let payload = self
                .read_payloads
                .first()
                .or(self.next_read_payload.as_ref());
            if !payload.is_some_and(|payload| payload.len() == data.buffer.len()) {
                return Err(sdmmc_host::SubmitTransactionError::new(
                    sdmmc_host::Error::Unsupported,
                    transaction,
                ));
            }
        }
        Ok(unsafe { self.submit_transaction(transaction) }
            .expect("owned MockHost submission was validated before mutation"))
    }

    fn advance_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        cause: sdmmc_host::ProgressCause,
    ) -> Result<sdmmc_host::RequestProgress<sdmmc_host::RawResponse>, sdmmc_host::AdvanceRequestError>
    where
        Self: 'a,
    {
        if self.complete_after_irq_register_retry {
            if cause == sdmmc_host::ProgressCause::AcknowledgedIrq {
                request.acknowledged_irq = true;
                return Ok(sdmmc_host::RequestProgress::RegisterPending {
                    retry_after: core::time::Duration::from_millis(1),
                });
            }
            if request.acknowledged_irq && cause == sdmmc_host::ProgressCause::RegisterRetry {
                return complete_mock_transaction(request);
            }
        }
        if cause != sdmmc_host::ProgressCause::AcknowledgedIrq {
            return Ok(sdmmc_host::RequestProgress::WaitingForIrq);
        }
        if self.pending_polls > 0 {
            self.pending_polls -= 1;
            return Ok(sdmmc_host::RequestProgress::WaitingForIrq);
        }
        complete_mock_transaction(request)
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), sdmmc_host::Error>
    where
        Self: 'a,
    {
        request.result = None;
        request.completed_dma = request
            .dma
            .take()
            .map(dma_api::PreparedDma::complete_without_device);
        Ok(())
    }

    fn take_completed_dma<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Option<dma_api::CompletedDma>
    where
        Self: 'a,
    {
        request.completed_dma.take()
    }

    unsafe fn submit_bus_op(
        &mut self,
        op: sdmmc_host::BusOp,
    ) -> Result<Self::BusRequest, sdmmc_host::Error> {
        match op {
            sdmmc_host::BusOp::SetBusWidth(width) => {
                if self.reject_bit8 && matches!(width, BusWidth::Bit8) {
                    return Err(sdmmc_host::Error::Unsupported);
                }
                self.bus_width = Some(width);
            }
            sdmmc_host::BusOp::SetClock(speed) => {
                self.last_clock = Some(speed);
                self.events.push(MockEvent::Clock(speed));
            }
            sdmmc_host::BusOp::SetSignalVoltage(voltage) => {
                self.last_voltage = Some(voltage);
                self.events.push(MockEvent::Voltage(voltage));
                if let Some(error) = self.voltage_switch_result {
                    return Err(protocol_error_to_host(error));
                }
            }
            sdmmc_host::BusOp::ExecuteTuning {
                command,
                block_size,
            } => {
                self.last_tuning = Some((command.index, block_size.get()));
                if let Some(error) = self.tuning_result {
                    return Err(protocol_error_to_host(error));
                }
            }
            _ => {}
        }
        let pending_advances = usize::from(
            self.multi_step_hs200_rollback
                && matches!(
                    op,
                    sdmmc_host::BusOp::SetSignalVoltage(SignalVoltage::V330)
                        | sdmmc_host::BusOp::SetClock(ClockSpeed::Default)
                ),
        );
        Ok(MockBusRequest {
            op,
            pending_advances,
            done: false,
        })
    }

    fn advance_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
        _cause: sdmmc_host::ProgressCause,
    ) -> Result<sdmmc_host::RequestProgress<()>, sdmmc_host::AdvanceRequestError> {
        if request.done {
            return Err(sdmmc_host::AdvanceRequestError::AlreadyCompleted);
        }
        if request.pending_advances > 0 {
            request.pending_advances -= 1;
            return Ok(sdmmc_host::RequestProgress::RegisterPending {
                retry_after: core::time::Duration::from_micros(10),
            });
        }
        request.done = true;
        Ok(sdmmc_host::RequestProgress::Complete(Ok(())))
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), sdmmc_host::Error> {
        self.aborted_bus_ops.push(request.op);
        request.done = true;
        Ok(())
    }

    fn now_ms(&self) -> Option<u64> {
        self.now_ms
    }
}

fn complete_mock_transaction(
    request: &mut MockTransactionRequest,
) -> Result<sdmmc_host::RequestProgress<sdmmc_host::RawResponse>, sdmmc_host::AdvanceRequestError> {
    let result = request
        .result
        .take()
        .ok_or(sdmmc_host::AdvanceRequestError::AlreadyCompleted)?;
    request.completed_dma = request
        .dma
        .take()
        .map(dma_api::PreparedDma::complete_without_device);
    Ok(sdmmc_host::RequestProgress::Complete(result))
}

struct MockIrq;

impl SdMmcIrqHandle for MockIrq {
    type Event = ();

    fn handle_irq(&mut self) -> Self::Event {}
}

impl SdMmcIrqHost for MockHost {
    type Event = ();
    type IrqHandle = MockIrq;
    type CardIrq = ();

    fn into_parts(self) -> sdmmc_host::HostParts<Self, Self::IrqHandle, Self::CardIrq> {
        sdmmc_host::HostParts {
            bus: self,
            irq: MockIrq,
            card_irq: None,
        }
    }

    fn completion_irq_enabled(&self) -> bool {
        self.completion_irq_enabled
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        self.completion_irq_enabled = true;
        Ok(())
    }

    fn rearm_completion_irq_and_check(&mut self) -> Result<CompletionIrqRearm, Error> {
        self.completion_irq_enabled = true;
        Ok(CompletionIrqRearm::Idle)
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        self.completion_irq_enabled = false;
        Ok(())
    }

    fn device_dma(&self) -> Result<&dma_api::DeviceDma, Error> {
        Ok(test_device_dma())
    }
}

struct TestDmaOp;

impl dma_api::DmaOp for TestDmaOp {
    fn page_size(&self) -> usize {
        4096
    }

    unsafe fn alloc_contiguous(
        &self,
        _constraints: dma_api::DmaConstraints,
        layout: Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) })?;
        Some(unsafe {
            dma_api::DmaAllocHandle::new(ptr, ptr, (ptr.as_ptr() as usize as u64).into(), layout)
        })
    }

    unsafe fn dealloc_contiguous(&self, handle: dma_api::DmaAllocHandle) {
        unsafe { dealloc(handle.as_ptr().as_ptr(), handle.layout()) };
    }

    unsafe fn alloc_coherent(
        &self,
        constraints: dma_api::DmaConstraints,
        layout: Layout,
    ) -> Option<dma_api::DmaAllocHandle> {
        unsafe { self.alloc_contiguous(constraints, layout) }
    }

    unsafe fn dealloc_coherent(
        &self,
        handle: dma_api::DmaAllocHandle,
    ) -> Result<(), dma_api::DmaError> {
        unsafe { self.dealloc_contiguous(handle) };
        Ok(())
    }

    unsafe fn map_streaming(
        &self,
        _constraints: dma_api::DmaConstraints,
        addr: NonNull<u8>,
        size: core::num::NonZeroUsize,
        _direction: dma_api::DmaDirection,
    ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
        let layout =
            Layout::from_size_align(size.get(), 1).map_err(dma_api::DmaError::LayoutError)?;
        Ok(unsafe {
            dma_api::DmaMapHandle::new(addr, (addr.as_ptr() as usize as u64).into(), layout, None)
        })
    }

    unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
}

fn test_device_dma() -> &'static dma_api::DeviceDma {
    static DEVICE: OnceLock<dma_api::DeviceDma> = OnceLock::new();
    static OP: TestDmaOp = TestDmaOp;
    DEVICE.get_or_init(|| {
        dma_api::DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::NonCoherent,
                dma_api::DmaConstraints::new(u64::MAX),
            ),
            &OP,
        )
    })
}

fn protocol_error_to_host(error: Error) -> sdmmc_host::Error {
    match error {
        Error::Busy => sdmmc_host::Error::Busy,
        Error::Timeout(_) => sdmmc_host::Error::Timeout,
        Error::Crc(_) => sdmmc_host::Error::Crc,
        Error::NoCard => sdmmc_host::Error::NoCard,
        Error::UnsupportedCommand => sdmmc_host::Error::Unsupported,
        Error::InvalidArgument => sdmmc_host::Error::InvalidArgument,
        Error::Misaligned => sdmmc_host::Error::Misaligned,
        _ => sdmmc_host::Error::Controller,
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

fn disable_speed_selection(driver: &mut SdMmcCard<MockHost>) {
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

fn poll_init_to_completion<H: SdMmcIrqHost + 'static>(
    driver: &mut SdMmcCard<H>,
) -> Result<CardInfo, Error> {
    poll_init_to_completion_with_preference(driver, CardInitPreference::SdFirst)
}

fn poll_init_to_completion_with_preference<H: SdMmcIrqHost + 'static>(
    driver: &mut SdMmcCard<H>,
    preference: CardInitPreference,
) -> Result<CardInfo, Error> {
    let mut request = driver.submit_init_with_preference(preference)?;
    loop {
        match advance_init_once(driver, &mut request)? {
            OperationProgress::Pending => {}
            OperationProgress::Complete(info) => return Ok(info),
        }
    }
}

fn advance_init_once<H: SdMmcIrqHost + 'static>(
    driver: &mut SdMmcCard<H>,
    request: &mut SdMmcInitRequest<H>,
) -> Result<OperationProgress<CardInfo>, Error> {
    let cause = match driver.init_wait_kind(request) {
        SdMmcInitWait::Irq => sdmmc_host::ProgressCause::AcknowledgedIrq,
        SdMmcInitWait::Register => sdmmc_host::ProgressCause::RegisterRetry,
    };
    driver.advance_init_request(request, cause)
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
