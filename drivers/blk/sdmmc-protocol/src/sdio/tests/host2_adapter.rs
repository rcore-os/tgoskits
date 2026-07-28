use super::*;

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

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), sdio_host2::Error> {
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
fn host2_sync_tuning_wrapper_rejects_irq_driven_phase() {
    let host = Host2Mock::new(sdio_host2::RawResponse::empty());
    let mut adapter = SdioHost2Adapter::new(host);

    assert_eq!(
        adapter.execute_tuning(21, NonZeroU16::new(64).unwrap()),
        Err(Error::UnsupportedCommand)
    );
    assert!(adapter.with_host(|host| host.bus_ops.is_empty()));
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
    host.bus_pending_polls = (SDIO_HOST2_REGISTER_SPIN_LIMIT as usize) + 1;
    let mut adapter = SdioHost2Adapter::new(host);

    assert!(matches!(
        adapter.set_clock(ClockSpeed::HighSpeed),
        Err(Error::Timeout(_))
    ));

    assert_eq!(adapter.with_host(|host| host.bus_aborts), 1);
}
