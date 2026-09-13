use alloc::boxed::Box;
use core::{fmt, time::Duration};

use dma_api::CompletedDma;

use crate::{
    bus::{BusOp, Error},
    command::{Command, RawResponse},
    data::DataPhase,
};

/// One physical bus transaction: a command and an optional data phase.
pub struct Transaction<'a> {
    pub command: Command,
    pub data: Option<DataPhase<'a>>,
}

impl<'a> Transaction<'a> {
    pub const fn command(command: Command) -> Self {
        Self {
            command,
            data: None,
        }
    }

    pub const fn with_data(command: Command, data: DataPhase<'a>) -> Self {
        Self {
            command,
            data: Some(data),
        }
    }
}

/// Submit failure for an owned transaction.
///
/// A rejected submission must return the original transaction. Once hardware
/// has accepted a transaction, the host must instead return a request and
/// report any later failure through [`RequestProgress::Complete`].
pub struct SubmitTransactionError<'a> {
    pub error: Error,
    transaction: Box<Transaction<'a>>,
}

impl<'a> SubmitTransactionError<'a> {
    pub fn new(error: Error, transaction: Transaction<'a>) -> Self {
        Self {
            error,
            transaction: Box::new(transaction),
        }
    }

    pub fn into_transaction(self) -> Transaction<'a> {
        *self.transaction
    }

    pub fn into_parts(self) -> (Error, Transaction<'a>) {
        (self.error, *self.transaction)
    }
}

/// Cause that permits a maintenance task to advance one host request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgressCause {
    /// The request was just accepted and hardware submission may start.
    Submitted,
    /// A hard IRQ handler acknowledged and latched a matching device event.
    AcknowledgedIrq,
    /// A bounded register-only wait expired.
    RegisterRetry,
}

/// Result of advancing a submitted request once.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestProgress<T> {
    /// A register-only transition may be retried after this delay.
    RegisterPending { retry_after: Duration },
    /// Command or data progress requires a matching acknowledged IRQ.
    WaitingForIrq,
    /// The request reached a terminal state and hardware no longer accesses
    /// its payload.
    Complete(Result<T, Error>),
}

/// Error returned when a request is advanced through the wrong handle or after
/// its terminal state.
///
/// Unlike [`RequestProgress::Complete`], this is not a transfer terminal state for
/// the request payload. Implementations must not report a terminal
/// [`RequestProgress::Complete`] error until the controller is no longer accessing
/// the transaction buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum AdvanceRequestError {
    WrongOwner,
    WrongKind,
    AlreadyCompleted,
    StaleGeneration,
    /// Recovery could not be reported through the requested handle.
    ///
    /// Safe host implementations must still quiesce the hardware before any
    /// request object that borrows caller memory can be dropped. This variant
    /// is diagnostic only; it must not mean DMA is still active.
    RecoveryFailed,
}

impl fmt::Display for AdvanceRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::WrongOwner => "request belongs to a different host",
            Self::WrongKind => "request was advanced through the wrong operation kind",
            Self::AlreadyCompleted => "request has already completed",
            Self::StaleGeneration => "request generation is no longer active",
            Self::RecoveryFailed => "request recovery failed",
        };
        f.write_str(s)
    }
}

impl core::error::Error for AdvanceRequestError {}

/// Physical SD/SDIO/MMC host bus.
///
/// The base contract is single active transaction: a host may reject a submit
/// with [`Error::Busy`] while another transaction or bus operation is active.
pub trait SdMmcHost {
    type TransactionRequest<'a>: Send
    where
        Self: 'a;
    type BusRequest: Send;

    /// Submit one CMD/DAT transaction.
    ///
    /// # Safety
    ///
    /// Callers must advance the returned request until
    /// [`RequestProgress::Complete`] or call [`Self::abort_transaction`] before
    /// dropping it. Until one of those terminal paths runs, the host may still
    /// access the associated data buffer through DMA or FIFO PIO.
    unsafe fn submit_transaction<'a>(
        &mut self,
        transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, Error>
    where
        Self: 'a;

    /// Submit one CMD/DAT transaction while preserving transaction ownership
    /// on submit-side failure.
    ///
    /// # Safety
    ///
    /// Same lifetime contract as [`Self::submit_transaction`].
    unsafe fn submit_transaction_owned<'a>(
        &mut self,
        transaction: Transaction<'a>,
    ) -> Result<Self::TransactionRequest<'a>, SubmitTransactionError<'a>>
    where
        Self: 'a;

    /// Advances one transaction for an explicit task-side cause.
    ///
    /// `Submitted` and `RegisterRetry` must never complete a CMD or DAT phase.
    /// Such phases may become terminal only when `cause` is
    /// [`ProgressCause::AcknowledgedIrq`].
    fn advance_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        cause: ProgressCause,
    ) -> Result<RequestProgress<RawResponse>, AdvanceRequestError>
    where
        Self: 'a;

    /// Abort a transaction.
    ///
    /// This is part of the safe lifetime contract for borrowed transaction
    /// buffers. Implementations may return an error to report that the
    /// controller had to be reset or poisoned, but before returning they must
    /// have stopped command/data engines and any DMA bus-master access that
    /// could still touch the request buffer.
    fn abort_transaction<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
    ) -> Result<(), Error>
    where
        Self: 'a;

    fn take_completed_dma<'a>(
        &mut self,
        _request: &mut Self::TransactionRequest<'a>,
    ) -> Option<CompletedDma>
    where
        Self: 'a,
    {
        None
    }

    /// Submit one non-data bus operation.
    ///
    /// # Safety
    ///
    /// The returned request must be advanced until
    /// [`RequestProgress::Complete`] or passed to [`Self::abort_bus_op`] before
    /// being dropped.
    unsafe fn submit_bus_op(&mut self, op: BusOp) -> Result<Self::BusRequest, Error>;

    fn advance_bus_op(
        &mut self,
        request: &mut Self::BusRequest,
        cause: ProgressCause,
    ) -> Result<RequestProgress<()>, AdvanceRequestError>;

    /// Abort a bus operation.
    ///
    /// Like [`Self::abort_transaction`], returning from this method means the
    /// controller is no longer executing the operation even when the return
    /// value carries a diagnostic error.
    fn abort_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<(), Error>;

    fn now_ms(&self) -> Option<u64> {
        None
    }
}
