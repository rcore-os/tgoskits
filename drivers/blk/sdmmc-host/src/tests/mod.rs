use core::num::{NonZeroU16, NonZeroU32};

use crate::*;

struct MockHost {
    busy: bool,
}

#[derive(Debug)]
struct MockTransactionRequest {
    response: RawResponse,
    done: bool,
}

#[derive(Debug)]
struct MockBusRequest {
    done: bool,
}

impl SdMmcHost for MockHost {
    type TransactionRequest<'a>
        = MockTransactionRequest
    where
        Self: 'a;
    type BusRequest = MockBusRequest;

    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, Error>
    where
        Self: 'a,
    {
        if self.busy {
            return Err(Error::Busy);
        }
        self.busy = true;
        Ok(MockTransactionRequest {
            response: RawResponse::new(transaction.command.response, [0x1234, 0, 0, 0]),
            done: false,
        })
    }

    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, SubmitTransactionError<'a>>
    where
        Self: 'a,
    {
        if self.busy {
            return Err(SubmitTransactionError::new(Error::Busy, transaction));
        }
        self.busy = true;
        Ok(MockTransactionRequest {
            response: RawResponse::new(transaction.command.response, [0x1234, 0, 0, 0]),
            done: false,
        })
    }

    fn advance_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        cause: ProgressCause,
    ) -> Result<RequestProgress<RawResponse>, AdvanceRequestError>
    where
        Self: 'a,
    {
        if request.done {
            return Err(AdvanceRequestError::AlreadyCompleted);
        }
        if cause != ProgressCause::AcknowledgedIrq {
            return Ok(RequestProgress::WaitingForIrq);
        }
        self.busy = false;
        request.done = true;
        Ok(RequestProgress::Complete(Ok(request.response)))
    }

    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), Error>
    where
        Self: 'a,
    {
        request.done = true;
        self.busy = false;
        Ok(())
    }

    unsafe fn submit_bus_op(&mut self, _op: BusOp) -> Result<Self::BusRequest, Error> {
        if self.busy {
            return Err(Error::Busy);
        }
        self.busy = true;
        Ok(MockBusRequest { done: false })
    }

    fn advance_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
        cause: ProgressCause,
    ) -> Result<RequestProgress<()>, AdvanceRequestError> {
        if request.done {
            return Err(AdvanceRequestError::AlreadyCompleted);
        }
        if cause != ProgressCause::AcknowledgedIrq {
            return Ok(RequestProgress::WaitingForIrq);
        }
        self.busy = false;
        request.done = true;
        Ok(RequestProgress::Complete(Ok(())))
    }

    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), Error> {
        request.done = true;
        self.busy = false;
        Ok(())
    }
}

#[test]
fn data_phase_validates_buffer_shape() {
    let mut read = [0u8; 1024];
    let block = NonZeroU16::new(512).unwrap();
    let phase = DataPhase::read(block, NonZeroU32::new(2).unwrap(), &mut read).unwrap();
    assert_eq!(phase.direction, DataDirection::Read);
    assert_eq!(phase.buffer.len(), 1024);
}

#[test]
fn host_reports_busy_for_second_active_transaction() {
    let mut host = MockHost { busy: false };
    let cmd = Command::new(17, 0, ResponseType::R1);
    let mut request = unsafe { host.submit_transaction(Transaction::command(cmd)) }.unwrap();
    assert_eq!(
        unsafe { host.submit_transaction(Transaction::command(cmd)) }.unwrap_err(),
        Error::Busy
    );
    assert_eq!(
        host.advance_transaction(&mut request, ProgressCause::Submitted),
        Ok(RequestProgress::WaitingForIrq)
    );
    assert!(matches!(
        host.advance_transaction(&mut request, ProgressCause::AcknowledgedIrq),
        Ok(RequestProgress::Complete(Ok(_)))
    ));
    assert_eq!(
        host.advance_transaction(&mut request, ProgressCause::AcknowledgedIrq),
        Err(AdvanceRequestError::AlreadyCompleted)
    );
    assert!(unsafe { host.submit_transaction(Transaction::command(cmd)) }.is_ok());
}

#[test]
fn bus_op_uses_same_single_active_contract() {
    let mut host = MockHost { busy: false };
    let _request = unsafe { host.submit_bus_op(BusOp::SetClock(ClockSpeed::Default)) }.unwrap();
    assert_eq!(
        unsafe { host.submit_bus_op(BusOp::SetBusWidth(BusWidth::Bit4)) }.unwrap_err(),
        Error::Busy
    );
}

#[test]
fn abort_releases_single_active_contract() {
    let mut host = MockHost { busy: false };
    let cmd = Command::new(17, 0, ResponseType::R1);
    let mut request = unsafe { host.submit_transaction(Transaction::command(cmd)) }.unwrap();

    host.abort_transaction(&mut request).unwrap();

    assert!(unsafe { host.submit_transaction(Transaction::command(cmd)) }.is_ok());
    assert_eq!(
        host.advance_transaction(&mut request, ProgressCause::AcknowledgedIrq),
        Err(AdvanceRequestError::AlreadyCompleted)
    );
}

#[test]
fn command_and_data_cannot_complete_without_acknowledged_irq() {
    let mut host = MockHost { busy: false };
    let command = Command::new(17, 0, ResponseType::R1);
    let mut request = unsafe { host.submit_transaction(Transaction::command(command)) }.unwrap();

    assert_eq!(
        host.advance_transaction(&mut request, ProgressCause::Submitted),
        Ok(RequestProgress::WaitingForIrq)
    );
    assert_eq!(
        host.advance_transaction(&mut request, ProgressCause::RegisterRetry),
        Ok(RequestProgress::WaitingForIrq)
    );
    assert_eq!(
        host.advance_transaction(&mut request, ProgressCause::AcknowledgedIrq),
        Ok(RequestProgress::Complete(Ok(RawResponse::new(
            ResponseType::R1,
            [0x1234, 0, 0, 0]
        ))))
    );
}
