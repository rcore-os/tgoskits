use super::*;
use crate::sdio::host2::ProtocolHost;

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
    transaction_advances: usize,
    bus_aborts: usize,
    completion_irq_enabled: bool,
    complete_without_irq: bool,
    complete_after_irq_register_retry: bool,
    register_wait: bool,
}

struct Host2TransactionRequest {
    response: sdio_host2::RawResponse,
    pending_polls: usize,
    acknowledged_irq: bool,
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
            acknowledged_irq: false,
            done: false,
        })
    }

    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: sdio_host2::Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, sdio_host2::SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        Ok(unsafe { self.submit_transaction(transaction) }
            .expect("Host2Mock does not reject submitted transactions"))
    }

    fn advance_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        cause: sdio_host2::ProgressCause,
    ) -> Result<sdio_host2::RequestProgress<sdio_host2::RawResponse>, sdio_host2::AdvanceRequestError>
    where
        Self: 'a,
    {
        self.transaction_advances += 1;
        if request.done {
            return Err(sdio_host2::AdvanceRequestError::AlreadyCompleted);
        }
        if self.complete_after_irq_register_retry {
            if cause == sdio_host2::ProgressCause::AcknowledgedIrq {
                request.acknowledged_irq = true;
                return Ok(sdio_host2::RequestProgress::RegisterPending {
                    retry_after: core::time::Duration::from_millis(1),
                });
            }
            if request.acknowledged_irq && cause == sdio_host2::ProgressCause::RegisterRetry {
                request.done = true;
                return Ok(sdio_host2::RequestProgress::Complete(Ok(request.response)));
            }
        }
        if cause != sdio_host2::ProgressCause::AcknowledgedIrq && !self.complete_without_irq {
            return Ok(sdio_host2::RequestProgress::WaitingForIrq);
        }
        if request.pending_polls > 0 {
            request.pending_polls -= 1;
            return Ok(sdio_host2::RequestProgress::WaitingForIrq);
        }
        if let Some(err) = self.transaction_error.take() {
            request.done = true;
            return Ok(sdio_host2::RequestProgress::Complete(Err(err)));
        }
        request.done = true;
        Ok(sdio_host2::RequestProgress::Complete(Ok(request.response)))
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

    fn advance_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
        _cause: sdio_host2::ProgressCause,
    ) -> Result<sdio_host2::RequestProgress<()>, sdio_host2::AdvanceRequestError> {
        if request.done {
            return Err(sdio_host2::AdvanceRequestError::AlreadyCompleted);
        }
        if request.pending_polls > 0 {
            request.pending_polls -= 1;
            return Ok(sdio_host2::RequestProgress::RegisterPending {
                retry_after: core::time::Duration::from_millis(1),
            });
        }
        if let Some(err) = self.bus_error.take() {
            request.done = true;
            return Ok(sdio_host2::RequestProgress::Complete(Err(err)));
        }
        request.done = true;
        Ok(sdio_host2::RequestProgress::Complete(Ok(())))
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), sdio_host2::Error> {
        if !request.done {
            self.bus_aborts += 1;
            request.done = true;
        }
        Ok(())
    }
}

impl SdioIrqHost for Host2Mock {
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

    fn device_dma(&self) -> Result<&dma_api::DeviceDma, Error> {
        Ok(test_device_dma())
    }

    fn progress_wait_kind(&self) -> HostProgressWait {
        if self.register_wait {
            HostProgressWait::Register {
                retry_after: core::time::Duration::from_millis(7),
            }
        } else {
            HostProgressWait::Irq
        }
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
            transaction_advances: 0,
            bus_aborts: 0,
            completion_irq_enabled: false,
            complete_without_irq: false,
            complete_after_irq_register_retry: false,
            register_wait: false,
        }
    }
}

#[test]
fn host2_adapter_reports_forwarded_completion_irq_state() {
    let host = Host2Mock::new(sdio_host2::RawResponse::empty());
    let mut adapter = ProtocolHost::new(host);

    assert!(!adapter.inner().completion_irq_enabled());
    adapter.inner_mut().enable_completion_irq().unwrap();
    assert!(adapter.inner().completion_irq_enabled());
    adapter.inner_mut().disable_completion_irq().unwrap();
    assert!(!adapter.inner().completion_irq_enabled());
}

#[test]
fn command_submission_restores_completion_irq_cleared_by_controller_reset() {
    let host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
    let mut adapter = ProtocolHost::new(host);

    assert!(!adapter.inner().completion_irq_enabled());
    adapter.submit_command(&crate::cmd::CMD0).unwrap();
    assert!(
        adapter.inner().completion_irq_enabled(),
        "an IRQ-owned command must restore the hardware completion mask after reset"
    );
}

#[test]
fn host2_adapter_submits_read_as_physical_transaction() {
    let host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
    let mut driver = SdioSdmmc::new(host);
    driver.high_capacity = true;
    let mut buf = [0u8; 512];

    let mut request = driver.submit_read_blocks_into(9, &mut buf).unwrap();
    assert!(matches!(
        driver
            .advance_data_request(&mut request, sdio_host2::ProgressCause::AcknowledgedIrq)
            .unwrap(),
        DataCommandProgress::Complete(Response::R1(_))
    ));

    let transactions = driver.host().transactions.clone();
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
    let mut driver = SdioSdmmc::new(host);

    driver
        .protocol_host_mut()
        .set_clock(ClockSpeed::HighSpeed)
        .expect("bus op completes");

    assert_eq!(
        driver.host().bus_ops.clone(),
        std::vec![sdio_host2::BusOp::SetClock(ClockSpeed::HighSpeed)]
    );
}

#[test]
fn host2_adapter_poll_error_releases_active_command() {
    let mut host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
    host.transaction_error = Some(sdio_host2::Error::Timeout);
    let mut adapter = ProtocolHost::new(host);
    let cmd = Command::new(13, 0, ResponseType::R1);

    adapter.submit_command(&cmd).unwrap();
    assert!(matches!(
        adapter.advance_command_response(sdio_host2::ProgressCause::AcknowledgedIrq),
        Err(Error::Timeout(_))
    ));

    adapter.submit_command(&cmd).unwrap();
}

#[test]
fn protocol_rejects_command_completion_without_acknowledged_irq() {
    let mut host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
    host.complete_without_irq = true;
    let mut protocol = ProtocolHost::new(host);
    let command = Command::new(13, 0, ResponseType::R1);

    protocol.submit_command(&command).unwrap();
    assert!(matches!(
        protocol.advance_command_response(sdio_host2::ProgressCause::Submitted),
        Err(Error::BusError(_))
    ));

    protocol.submit_command(&command).unwrap();
}

#[test]
fn protocol_accepts_register_completion_after_same_request_irq() {
    let mut host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
    host.complete_after_irq_register_retry = true;
    let mut protocol = ProtocolHost::new(host);
    let command = Command::new(6, 0, ResponseType::R1b);

    protocol.submit_command(&command).unwrap();
    assert!(matches!(
        protocol
            .advance_command_response(sdio_host2::ProgressCause::AcknowledgedIrq)
            .unwrap(),
        crate::CommandResponseProgress::Pending
    ));
    assert!(matches!(
        protocol
            .advance_command_response(sdio_host2::ProgressCause::RegisterRetry)
            .unwrap(),
        crate::CommandResponseProgress::Complete(Response::R1(_))
    ));
}

#[test]
fn init_honors_host_register_wait_inside_an_irq_protocol_state() {
    let mut host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
    host.register_wait = true;
    let mut driver = SdioSdmmc::new(host);
    let scratch = SdioInitScratch::new(test_device_dma()).unwrap();
    let mut request = SdioInitRequest::new(CardInitPreference::SdOnly, scratch);
    request.state = SdioInitState::PollCmd0;
    driver
        .protocol_host_mut()
        .submit_command(&crate::cmd::CMD0)
        .unwrap();

    assert!(matches!(
        driver
            .advance_init_request(&mut request, sdio_host2::ProgressCause::RegisterRetry)
            .unwrap(),
        OperationProgress::Pending
    ));
    assert_eq!(
        driver.host().transaction_advances,
        1,
        "register-only command setup must advance without waiting for an IRQ that cannot exist yet"
    );
}

#[test]
fn init_preserves_irq_ack_while_command_start_needs_register_retry() {
    let mut host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
    host.register_wait = true;
    host.complete_after_irq_register_retry = true;
    let mut driver = SdioSdmmc::new(host);
    let scratch = SdioInitScratch::new(test_device_dma()).unwrap();
    let mut request = SdioInitRequest::new(CardInitPreference::SdOnly, scratch);
    request.state = SdioInitState::PollCmd0;
    driver
        .protocol_host_mut()
        .submit_command(&crate::cmd::CMD0)
        .unwrap();

    assert_eq!(driver.init_wait_kind(&request), SdioInitWait::Register);
    assert!(matches!(
        driver
            .advance_init_request(&mut request, sdio_host2::ProgressCause::AcknowledgedIrq)
            .unwrap(),
        OperationProgress::Pending
    ));
    assert_eq!(
        driver.host().transaction_advances,
        1,
        "an acknowledged IRQ belongs to the protocol command even while its START bit needs a \
         register retry"
    );

    assert!(matches!(
        driver
            .advance_init_request(&mut request, sdio_host2::ProgressCause::RegisterRetry)
            .unwrap(),
        OperationProgress::Pending
    ));
    assert_eq!(
        driver
            .host()
            .transactions
            .iter()
            .map(|(command, _)| command.index)
            .collect::<Vec<_>>(),
        std::vec![0, 8],
        "the retained IRQ acknowledgement must let the bounded register retry finish CMD0"
    );
}

#[test]
fn host2_sync_bus_wrapper_refuses_to_poll_pending_request() {
    let mut host = Host2Mock::new(sdio_host2::RawResponse::empty());
    host.bus_pending_polls = 3;
    let mut driver = SdioSdmmc::new(host);

    assert_eq!(
        driver.protocol_host_mut().set_clock(ClockSpeed::HighSpeed),
        Err(Error::Busy)
    );

    assert_eq!(
        driver.host().bus_ops.clone(),
        std::vec![sdio_host2::BusOp::SetClock(ClockSpeed::HighSpeed)]
    );
    assert_eq!(driver.host().bus_aborts, 1);
}

#[test]
fn host2_init_bus_op_pending_is_observed_without_spinning() {
    let mut host = Host2Mock::new(sdio_host2::RawResponse::empty());
    host.bus_pending_polls = 1;
    let mut driver = SdioSdmmc::new(host);

    let mut request = driver.submit_init().unwrap();
    assert!(driver.host().bus_ops.is_empty());

    assert!(matches!(
        advance_init_once(&mut driver, &mut request).unwrap(),
        OperationProgress::Pending
    ));
    assert_eq!(
        driver.host().bus_ops.clone(),
        std::vec![sdio_host2::BusOp::ResetAll]
    );
    assert!(driver.host().transactions.is_empty());

    assert!(matches!(
        advance_init_once(&mut driver, &mut request).unwrap(),
        OperationProgress::Pending
    ));
    assert_eq!(
        driver.init_register_retry_after(&request),
        Some(core::time::Duration::from_millis(1)),
        "the protocol adapter must preserve the driver's exact register retry delay"
    );
    assert_eq!(driver.host().bus_ops.len(), 1);
    assert!(driver.host().transactions.is_empty());

    assert!(matches!(
        advance_init_once(&mut driver, &mut request).unwrap(),
        OperationProgress::Pending
    ));
    assert_eq!(driver.host().bus_ops.len(), 1);
    assert!(driver.host().transactions.is_empty());

    assert!(matches!(
        advance_init_once(&mut driver, &mut request).unwrap(),
        OperationProgress::Pending
    ));
    assert_eq!(
        driver.host().bus_ops.clone(),
        std::vec![sdio_host2::BusOp::ResetAll, sdio_host2::BusOp::PowerOn]
    );
    assert!(driver.host().transactions.is_empty());
}

#[test]
fn host2_init_starts_with_physical_bus_ops_before_cmd0() {
    let host = Host2Mock::new(sdio_host2::RawResponse::empty());
    let mut driver = SdioSdmmc::new(host);
    let mut request = driver.submit_init().unwrap();

    for _ in 0..16 {
        assert!(matches!(
            advance_init_once(&mut driver, &mut request).unwrap(),
            OperationProgress::Pending
        ));
        if !driver.host().transactions.is_empty() {
            break;
        }
    }

    assert_eq!(
        driver.host().bus_ops.clone(),
        std::vec![
            sdio_host2::BusOp::ResetAll,
            sdio_host2::BusOp::PowerOn,
            sdio_host2::BusOp::SetSignalVoltage(SignalVoltage::V330),
            sdio_host2::BusOp::SetBusWidth(BusWidth::Bit1),
            sdio_host2::BusOp::SetClock(ClockSpeed::Identification),
        ]
    );
    let transactions = driver.host().transactions.clone();
    assert_eq!(transactions.len(), 1);
    assert_eq!(transactions[0].0.index, 0);
    assert!(transactions[0].1.is_none());
}

#[test]
fn host2_init_bus_op_error_releases_request_slot() {
    let mut host = Host2Mock::new(sdio_host2::RawResponse::empty());
    host.bus_error = Some(sdio_host2::Error::Timeout);
    let mut driver = SdioSdmmc::new(host);
    let mut request = driver.submit_init().unwrap();

    assert!(matches!(
        advance_init_once(&mut driver, &mut request).unwrap(),
        OperationProgress::Pending
    ));
    assert!(matches!(
        advance_init_once(&mut driver, &mut request),
        Err(Error::Timeout(_))
    ));
    assert!(request.bus_request.is_none());
}

#[test]
fn aborting_init_releases_active_bus_request() {
    let mut host = Host2Mock::new(sdio_host2::RawResponse::empty());
    host.bus_pending_polls = 1;
    let mut driver = SdioSdmmc::new(host);
    let mut request = driver.submit_init().unwrap();

    assert!(matches!(
        advance_init_once(&mut driver, &mut request).unwrap(),
        OperationProgress::Pending
    ));
    driver.host_mut().bus_pending_polls = 0;
    driver.abort_init_request(&mut request).unwrap();

    assert_eq!(driver.host().bus_aborts, 1);
    assert!(request.bus_request.is_none());
}

#[test]
fn aborting_status_barrier_releases_active_command() {
    let host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
    let mut driver = SdioSdmmc::new(host);
    let mut request = driver.submit_status().unwrap();

    driver.abort_status_request(&mut request).unwrap();

    assert_eq!(driver.host().transaction_aborts, 1);
}

#[test]
fn host2_adapter_explicit_abort_returns_pending_data_request() {
    let host = Host2Mock::new(ok_r1().to_raw_response(ResponseType::R1));
    let mut adapter = ProtocolHost::new(host);
    let cmd = Command::new(17, 0, ResponseType::R1);
    let mut buf = [0u8; 512];

    let mut request = adapter.submit_read_data(&cmd, &mut buf, 512, 1).unwrap();
    adapter.abort_data_request(&mut request).unwrap();

    assert_eq!(adapter.inner().transaction_aborts, 1);
}

#[test]
fn owned_dma_is_not_completed_without_irq_and_abort_returns_ownership() {
    let mut host = MockHost::new(std::vec![ok_r1()]);
    host.next_read_payload = Some(std::vec![0x5a; 512]);
    let mut protocol = ProtocolHost::new(host);
    let command = Command::new(17, 0, ResponseType::R1);
    let buffer = dma_api::CpuDmaBuffer::new_zero(
        test_device_dma(),
        core::num::NonZeroUsize::new(512).unwrap(),
        512,
        dma_api::DmaDirection::FromDevice,
    )
    .unwrap()
    .prepare_for_device();
    let mut request = protocol
        .submit_dma_data(&command, sdio_host2::DataDirection::Read, buffer, 512, 1)
        .unwrap_or_else(|_| panic!("valid owned DMA request must submit"));

    assert!(matches!(
        protocol
            .advance_data_request(&mut request, sdio_host2::ProgressCause::Submitted)
            .unwrap(),
        DataCommandProgress::Pending
    ));
    assert!(request.take_completed_dma().is_none());
    assert!(matches!(
        protocol
            .advance_data_request(&mut request, sdio_host2::ProgressCause::RegisterRetry)
            .unwrap(),
        DataCommandProgress::Pending
    ));
    assert!(request.take_completed_dma().is_none());

    protocol.abort_data_request(&mut request).unwrap();
    assert_eq!(request.take_completed_dma().unwrap().len().get(), 512);
}

#[test]
fn rejected_owned_dma_submission_returns_prepared_buffer() {
    let host = MockHost::new(std::vec![ok_r1()]);
    let mut protocol = ProtocolHost::new(host);
    let command = Command::new(17, 0, ResponseType::R1);
    let buffer = dma_api::CpuDmaBuffer::new_zero(
        test_device_dma(),
        core::num::NonZeroUsize::new(512).unwrap(),
        512,
        dma_api::DmaDirection::FromDevice,
    )
    .unwrap()
    .prepare_for_device();

    let error = protocol
        .submit_dma_data(&command, sdio_host2::DataDirection::Read, buffer, 512, 1)
        .err()
        .expect("missing read payload rejects the test submission");

    assert_eq!(error.error, Error::UnsupportedCommand);
    assert_eq!(error.into_buffer().len().get(), 512);
}

#[test]
fn host2_sync_bus_pending_is_not_spin_polled() {
    let mut host = Host2Mock::new(sdio_host2::RawResponse::empty());
    host.bus_pending_polls = 1;
    let mut adapter = ProtocolHost::new(host);

    assert!(matches!(
        adapter.set_clock(ClockSpeed::HighSpeed),
        Err(Error::Busy)
    ));

    assert_eq!(adapter.inner().bus_aborts, 1);
}
