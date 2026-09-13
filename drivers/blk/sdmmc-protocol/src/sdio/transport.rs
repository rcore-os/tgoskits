//! Private protocol state owner over one portable SD/MMC host.

use alloc::boxed::Box;
use core::num::{NonZeroU16, NonZeroU32};

use dma_api::{CompletedDma, PreparedDma};
use log::warn;
use sdmmc_host::{AdvanceRequestError, DataDirection, ProgressCause, RequestProgress, Transaction};

use super::host::{
    BusWidth, ClockSpeed, HostProgressWait, SdMmcBusOp, SdMmcIrqHost, SignalVoltage,
};
use crate::{
    block::{CommandResponseProgress, DataCommandProgress, OperationProgress},
    cmd::Command,
    error::{Error, ErrorContext},
};

pub(crate) struct DmaSubmitError {
    pub error: Error,
    buffer: Box<PreparedDma>,
}

impl DmaSubmitError {
    fn new(error: Error, buffer: PreparedDma) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    pub(crate) fn into_buffer(self) -> PreparedDma {
        *self.buffer
    }
}

/// Sole mutable owner of one physical host inside the protocol engine.
pub(crate) struct ProtocolHost<H: SdMmcIrqHost + 'static> {
    host: H,
    command_request: Option<H::TransactionRequest<'static>>,
    command_irq_acknowledged: bool,
    progress_wait: HostProgressWait,
}

pub(crate) struct ProtocolDataRequest<'a, H: SdMmcIrqHost + 'static> {
    inner: Option<H::TransactionRequest<'a>>,
    irq_acknowledged: bool,
    completed_dma: Option<CompletedDma>,
}

pub(crate) struct ProtocolBusRequest<H: SdMmcIrqHost + 'static> {
    inner: Option<H::BusRequest>,
    op: sdmmc_host::BusOp,
}

impl<H: SdMmcIrqHost + 'static> ProtocolHost<H> {
    pub(super) fn new(host: H) -> Self {
        Self {
            host,
            command_request: None,
            command_irq_acknowledged: false,
            progress_wait: HostProgressWait::Irq,
        }
    }

    pub(super) const fn inner(&self) -> &H {
        &self.host
    }

    pub(super) fn inner_mut(&mut self) -> &mut H {
        &mut self.host
    }

    pub(super) const fn progress_wait(&self) -> HostProgressWait {
        self.progress_wait
    }

    fn ensure_completion_irq(&mut self) -> Result<(), Error> {
        if self.host.completion_irq_enabled() {
            return Ok(());
        }
        self.host.enable_completion_irq()?;
        if self.host.completion_irq_enabled() {
            Ok(())
        } else {
            Err(Error::UnsupportedCommand)
        }
    }

    pub(super) fn submit_command(&mut self, command: &Command) -> Result<(), Error> {
        if self.command_request.is_some() {
            return Err(Error::Busy);
        }
        self.ensure_completion_irq()?;
        self.command_irq_acknowledged = false;
        let request = unsafe { self.host.submit_transaction(Transaction::command(*command)) }
            .map_err(host_error)?;
        self.progress_wait = self.host.progress_wait_kind();
        self.command_request = Some(request);
        Ok(())
    }

    pub(super) fn advance_command_response(
        &mut self,
        cause: ProgressCause,
    ) -> Result<CommandResponseProgress, Error> {
        self.command_irq_acknowledged |= cause == ProgressCause::AcknowledgedIrq;
        let mut request = self.command_request.take().ok_or(Error::InvalidArgument)?;
        match self.host.advance_transaction(&mut request, cause) {
            Ok(RequestProgress::RegisterPending { retry_after }) => {
                self.progress_wait = HostProgressWait::Register { retry_after };
                self.command_request = Some(request);
                Ok(CommandResponseProgress::Pending)
            }
            Ok(RequestProgress::WaitingForIrq) => {
                self.progress_wait = HostProgressWait::Irq;
                self.command_request = Some(request);
                Ok(CommandResponseProgress::Pending)
            }
            Ok(RequestProgress::Complete(Ok(_))) if !self.command_irq_acknowledged => {
                self.command_irq_acknowledged = false;
                self.progress_wait = HostProgressWait::Irq;
                Err(non_irq_completion_error())
            }
            Ok(RequestProgress::Complete(Ok(raw))) => {
                self.command_irq_acknowledged = false;
                self.progress_wait = HostProgressWait::Irq;
                crate::response::response_from_raw(raw).map(CommandResponseProgress::Complete)
            }
            Ok(RequestProgress::Complete(Err(error))) => {
                self.command_irq_acknowledged = false;
                self.progress_wait = HostProgressWait::Irq;
                Err(host_error(error))
            }
            Err(error) => {
                self.command_irq_acknowledged = false;
                self.progress_wait = HostProgressWait::Irq;
                let recovery = self
                    .host
                    .abort_transaction(&mut request)
                    .map_err(host_error);
                recovery.and(Err(advance_error(error)))
            }
        }
    }

    pub(crate) fn abort_command_request(&mut self) -> Result<(), Error> {
        let Some(mut request) = self.command_request.take() else {
            return Ok(());
        };
        self.command_irq_acknowledged = false;
        self.progress_wait = HostProgressWait::Irq;
        self.host
            .abort_transaction(&mut request)
            .map_err(host_error)
    }

    pub(super) fn submit_read_data<'a>(
        &mut self,
        command: &Command,
        buffer: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<ProtocolDataRequest<'a, H>, Error> {
        let data = sdmmc_host::DataPhase::read(
            nonzero_block_size(block_size)?,
            nonzero_block_count(block_count)?,
            buffer,
        )
        .map_err(host_error)?;
        self.submit_data(command, data)
    }

    pub(super) fn submit_write_data<'a>(
        &mut self,
        command: &Command,
        buffer: &'a [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<ProtocolDataRequest<'a, H>, Error> {
        let data = sdmmc_host::DataPhase::write(
            nonzero_block_size(block_size)?,
            nonzero_block_count(block_count)?,
            buffer,
        )
        .map_err(host_error)?;
        self.submit_data(command, data)
    }

    fn submit_data<'a>(
        &mut self,
        command: &Command,
        data: sdmmc_host::DataPhase<'a>,
    ) -> Result<ProtocolDataRequest<'a, H>, Error> {
        self.ensure_completion_irq()?;
        let request = unsafe {
            self.host
                .submit_transaction(Transaction::with_data(*command, data))
        }
        .map_err(host_error)?;
        self.progress_wait = self.host.progress_wait_kind();
        Ok(ProtocolDataRequest {
            inner: Some(request),
            irq_acknowledged: false,
            completed_dma: None,
        })
    }

    pub(crate) fn advance_data_request(
        &mut self,
        request: &mut ProtocolDataRequest<'_, H>,
        cause: ProgressCause,
    ) -> Result<DataCommandProgress, Error> {
        request.irq_acknowledged |= cause == ProgressCause::AcknowledgedIrq;
        let inner = request.inner.as_mut().ok_or(Error::InvalidArgument)?;
        match self.host.advance_transaction(inner, cause) {
            Ok(RequestProgress::RegisterPending { retry_after }) => {
                self.progress_wait = HostProgressWait::Register { retry_after };
                Ok(DataCommandProgress::Pending)
            }
            Ok(RequestProgress::WaitingForIrq) => {
                self.progress_wait = HostProgressWait::Irq;
                Ok(DataCommandProgress::Pending)
            }
            Ok(RequestProgress::Complete(Ok(_))) if !request.irq_acknowledged => {
                self.progress_wait = HostProgressWait::Irq;
                request.completed_dma = request
                    .inner
                    .as_mut()
                    .and_then(|inner| self.host.take_completed_dma(inner));
                request.inner = None;
                Err(non_irq_completion_error())
            }
            Ok(RequestProgress::Complete(Ok(raw))) => {
                self.progress_wait = HostProgressWait::Irq;
                request.completed_dma = request
                    .inner
                    .as_mut()
                    .and_then(|inner| self.host.take_completed_dma(inner));
                request.inner = None;
                crate::response::response_from_raw(raw).map(DataCommandProgress::Complete)
            }
            Ok(RequestProgress::Complete(Err(error))) => {
                self.progress_wait = HostProgressWait::Irq;
                request.completed_dma = request
                    .inner
                    .as_mut()
                    .and_then(|inner| self.host.take_completed_dma(inner));
                request.inner = None;
                Err(host_error(error))
            }
            Err(error) => {
                self.progress_wait = HostProgressWait::Irq;
                let recovery = self.host.abort_transaction(inner).map_err(host_error);
                request.completed_dma = self.host.take_completed_dma(inner);
                request.inner = None;
                recovery.and(Err(advance_error(error)))
            }
        }
    }

    pub(crate) fn abort_data_request(
        &mut self,
        request: &mut ProtocolDataRequest<'_, H>,
    ) -> Result<(), Error> {
        let Some(mut inner) = request.inner.take() else {
            return Ok(());
        };
        let result = self.host.abort_transaction(&mut inner).map_err(host_error);
        self.progress_wait = HostProgressWait::Irq;
        request.completed_dma = self.host.take_completed_dma(&mut inner);
        result
    }

    pub(super) fn submit_bus_op(&mut self, op: SdMmcBusOp) -> Result<ProtocolBusRequest<H>, Error> {
        let op = op.into_host_op();
        let inner = unsafe { self.host.submit_bus_op(op) }.map_err(host_error)?;
        // A newly submitted bus operation has not run its first owner step, so
        // it cannot yet know whether hardware IRQ or register progress is
        // required. Execute that Submitted step immediately in owner task
        // context; `advance_bus_op` will publish the host's actual next wait.
        self.progress_wait = HostProgressWait::Register {
            retry_after: core::time::Duration::ZERO,
        };
        Ok(ProtocolBusRequest {
            inner: Some(inner),
            op,
        })
    }

    pub(super) fn advance_bus_op(
        &mut self,
        request: &mut ProtocolBusRequest<H>,
        cause: ProgressCause,
    ) -> Result<OperationProgress<()>, Error> {
        let inner = request.inner.as_mut().ok_or(Error::InvalidArgument)?;
        match self.host.advance_bus_op(inner, cause) {
            Ok(RequestProgress::RegisterPending { retry_after }) => {
                self.progress_wait = HostProgressWait::Register { retry_after };
                Ok(OperationProgress::Pending)
            }
            Ok(RequestProgress::WaitingForIrq) => {
                self.progress_wait = HostProgressWait::Irq;
                Ok(OperationProgress::Pending)
            }
            Ok(RequestProgress::Complete(Ok(()))) => {
                self.progress_wait = HostProgressWait::Irq;
                request.inner = None;
                Ok(OperationProgress::Complete(()))
            }
            Ok(RequestProgress::Complete(Err(error))) => {
                self.progress_wait = HostProgressWait::Irq;
                request.inner = None;
                Err(host_error(error))
            }
            Err(error) => {
                self.progress_wait = HostProgressWait::Irq;
                warn!("SD/MMC bus op {:?} advance failed: {error:?}", request.op);
                let recovery = self.host.abort_bus_op(inner).map_err(host_error);
                request.inner = None;
                recovery.and(Err(advance_error(error)))
            }
        }
    }

    pub(crate) fn abort_bus_request(
        &mut self,
        request: &mut ProtocolBusRequest<H>,
    ) -> Result<(), Error> {
        let Some(mut inner) = request.inner.take() else {
            return Ok(());
        };
        self.progress_wait = HostProgressWait::Irq;
        self.host.abort_bus_op(&mut inner).map_err(host_error)
    }

    fn run_register_op_once(&mut self, op: SdMmcBusOp) -> Result<(), Error> {
        let mut request = self.submit_bus_op(op)?;
        match self.advance_bus_op(&mut request, ProgressCause::RegisterRetry)? {
            OperationProgress::Complete(()) => Ok(()),
            OperationProgress::Pending => {
                self.abort_bus_request(&mut request)?;
                Err(Error::Busy)
            }
        }
    }

    pub(super) fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
        self.run_register_op_once(SdMmcBusOp::SetBusWidth(width))
    }

    pub(super) fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
        self.run_register_op_once(SdMmcBusOp::SetClock(speed))
    }

    pub(super) fn switch_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        self.run_register_op_once(SdMmcBusOp::SwitchVoltage(voltage))
    }

    pub(super) fn now_ms(&self) -> Option<u64> {
        self.host.now_ms()
    }

    pub(crate) fn submit_dma_data(
        &mut self,
        command: &Command,
        direction: DataDirection,
        buffer: PreparedDma,
        block_size: u32,
        block_count: u32,
    ) -> Result<ProtocolDataRequest<'static, H>, DmaSubmitError> {
        if let Err(error) = self.ensure_completion_irq() {
            return Err(DmaSubmitError::new(error, buffer));
        }
        let block_size = match nonzero_block_size(block_size) {
            Ok(block_size) => block_size,
            Err(error) => return Err(DmaSubmitError::new(error, buffer)),
        };
        let block_count = match nonzero_block_count(block_count) {
            Ok(block_count) => block_count,
            Err(error) => return Err(DmaSubmitError::new(error, buffer)),
        };
        let data = sdmmc_host::DataPhase::dma(direction, block_size, block_count, buffer).map_err(
            |error| {
                let (error, buffer) = error.into_parts();
                DmaSubmitError::new(host_error(error), buffer)
            },
        )?;
        let transaction = Transaction::with_data(*command, data);
        match unsafe { self.host.submit_transaction_owned(transaction) } {
            Ok(request) => {
                self.progress_wait = self.host.progress_wait_kind();
                Ok(ProtocolDataRequest {
                    inner: Some(request),
                    irq_acknowledged: false,
                    completed_dma: None,
                })
            }
            Err(error) => {
                let protocol_error = host_error(error.error);
                let transaction = error.into_transaction();
                let Some(buffer) = recover_dma_buffer(transaction) else {
                    panic!("SD/MMC DMA submit failure did not return DMA ownership");
                };
                Err(DmaSubmitError::new(protocol_error, buffer))
            }
        }
    }
}

impl<H: SdMmcIrqHost + 'static> Drop for ProtocolHost<H> {
    fn drop(&mut self) {
        if let Err(error) = self.abort_command_request() {
            warn!("SD/MMC pending command recovery failed during teardown: {error:?}");
        }
    }
}

impl<H: SdMmcIrqHost + 'static> ProtocolDataRequest<'_, H> {
    pub(crate) fn take_completed_dma(&mut self) -> Option<CompletedDma> {
        self.completed_dma.take()
    }
}

fn recover_dma_buffer(transaction: Transaction<'_>) -> Option<PreparedDma> {
    match transaction.data?.buffer {
        sdmmc_host::DataBuffer::Dma(buffer) => Some(buffer),
        sdmmc_host::DataBuffer::Read(_) | sdmmc_host::DataBuffer::Write(_) => None,
    }
}

fn nonzero_block_size(block_size: u32) -> Result<NonZeroU16, Error> {
    u16::try_from(block_size)
        .ok()
        .and_then(NonZeroU16::new)
        .ok_or(Error::InvalidArgument)
}

fn nonzero_block_count(block_count: u32) -> Result<NonZeroU32, Error> {
    NonZeroU32::new(block_count).ok_or(Error::InvalidArgument)
}

fn host_error(error: sdmmc_host::Error) -> Error {
    match error {
        sdmmc_host::Error::Busy => Error::Busy,
        sdmmc_host::Error::Timeout => Error::Timeout(ErrorContext::default()),
        sdmmc_host::Error::Crc => Error::Crc(ErrorContext::default()),
        sdmmc_host::Error::NoCard => Error::NoCard,
        sdmmc_host::Error::Unsupported => Error::UnsupportedCommand,
        sdmmc_host::Error::InvalidArgument => Error::InvalidArgument,
        sdmmc_host::Error::Misaligned => Error::Misaligned,
        sdmmc_host::Error::Bus | sdmmc_host::Error::Controller => {
            Error::BusError(ErrorContext::default())
        }
        _ => Error::BusError(ErrorContext::default()),
    }
}

fn advance_error(error: AdvanceRequestError) -> Error {
    match error {
        AdvanceRequestError::AlreadyCompleted => Error::InvalidArgument,
        AdvanceRequestError::WrongOwner
        | AdvanceRequestError::WrongKind
        | AdvanceRequestError::StaleGeneration
        | AdvanceRequestError::RecoveryFailed => Error::BusError(ErrorContext::default()),
        _ => Error::BusError(ErrorContext::default()),
    }
}

fn non_irq_completion_error() -> Error {
    Error::BusError(ErrorContext::default())
}
