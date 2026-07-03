//! Compatibility adapter between `sdio-host2` and the protocol `SdioHost` trait.

#[cfg(feature = "rdif")]
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    num::{NonZeroU16, NonZeroU32},
    sync::atomic::{AtomicBool, Ordering},
};

use dma_api::CompletedDma;
#[cfg(feature = "rdif")]
use dma_api::PreparedDma;
use log::{debug, warn};

use super::{
    card::SdioSdmmc,
    host::{
        BusWidth, ClockSpeed, HostEvent, SdioBusOp, SdioHost, SdioIrqHandle, SdioIrqHost,
        SignalVoltage,
    },
};
use crate::{
    block::{CommandResponsePoll, DataCommandPoll, OperationPoll},
    cmd::Command,
    error::{Error, ErrorContext, Phase},
    response::ResponseType,
};

#[cfg(feature = "rdif")]
pub(crate) struct DmaSubmitError {
    pub error: Error,
    buffer: Box<PreparedDma>,
}

#[cfg(feature = "rdif")]
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

pub struct SdioHost2Adapter<H: SdioHost2Irq + 'static> {
    core: Host2Shared<H>,
    command_request: Option<H::TransactionRequest<'static>>,
}

/// IRQ-capable extension used by [`SdioHost2Adapter`].
///
/// `sdio-host2` intentionally does not define IRQ abstractions. This protocol
/// crate only needs a way to forward host-specific completion IRQ handles when
/// a physical host is wrapped for the legacy `SdioHost` card state machine.
pub trait SdioHost2Irq: sdio_host2::SdioHost {
    type Event: HostEvent + Default;
    type IrqHandle: SdioIrqHandle<Event = Self::Event>;

    fn completion_irq_enabled(&self) -> bool {
        false
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn irq_handle(&mut self) -> Self::IrqHandle;
}

impl<T> SdioHost2Irq for T
where
    T: sdio_host2::SdioHost + SdioIrqHost,
{
    type Event = <T as SdioHost>::Event;
    type IrqHandle = <T as SdioIrqHost>::IrqHandle;

    fn completion_irq_enabled(&self) -> bool {
        SdioHost::completion_irq_enabled(self)
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        SdioHost::enable_completion_irq(self)
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        SdioHost::disable_completion_irq(self)
    }

    fn irq_handle(&mut self) -> Self::IrqHandle {
        SdioIrqHost::irq_handle(self)
    }
}

struct Host2Shared<H> {
    inner: Arc<Host2SharedInner<H>>,
}

struct Host2SharedInner<H> {
    host: UnsafeCell<H>,
    borrowed: AtomicBool,
}

// SAFETY: Access to `host` is serialized by `borrowed`; the wrapper never
// hands out references that outlive a `with_*` call.
unsafe impl<H: Send> Send for Host2SharedInner<H> {}
// SAFETY: See the `Send` impl.
unsafe impl<H: Send> Sync for Host2SharedInner<H> {}

impl<H> Clone for Host2Shared<H> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<H> Host2Shared<H> {
    fn new(host: H) -> Self {
        Self {
            inner: Arc::new(Host2SharedInner {
                host: UnsafeCell::new(host),
                borrowed: AtomicBool::new(false),
            }),
        }
    }

    fn with_ref<R>(&self, f: impl FnOnce(&H) -> R) -> R {
        self.borrow(|host| f(host))
    }

    fn with_mut<R>(&self, f: impl FnOnce(&mut H) -> R) -> R {
        self.borrow(|host| f(host))
    }

    fn borrow<R>(&self, f: impl FnOnce(&mut H) -> R) -> R {
        if self
            .inner
            .borrowed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            panic!("sdio-host2 adapter host borrowed concurrently");
        }
        struct BorrowGuard<'a>(&'a AtomicBool);
        impl Drop for BorrowGuard<'_> {
            fn drop(&mut self) {
                self.0.store(false, Ordering::Release);
            }
        }
        let _guard = BorrowGuard(&self.inner.borrowed);
        // SAFETY: the atomic guard above serializes access to the host.
        f(unsafe { &mut *self.inner.host.get() })
    }
}

impl<H: SdioHost2Irq + 'static> SdioHost2Adapter<H> {
    pub fn new(host: H) -> Self {
        Self {
            core: Host2Shared::new(host),
            command_request: None,
        }
    }

    pub fn with_host<R>(&self, f: impl FnOnce(&H) -> R) -> R {
        self.core.with_ref(f)
    }

    pub fn with_host_mut<R>(&self, f: impl FnOnce(&mut H) -> R) -> R {
        self.core.with_mut(f)
    }

    fn drain_bus_op(&mut self, request: &mut SdioHost2BusRequest<H>) -> Result<(), Error> {
        for _ in 0..SDIO_HOST2_COMPAT_POLL_LIMIT {
            match self.poll_bus_op(request)? {
                OperationPoll::Pending => core::hint::spin_loop(),
                OperationPoll::Complete(()) => return Ok(()),
            }
        }
        request.abort()?;
        Err(Error::Timeout(ErrorContext::new(Phase::Init)))
    }
}

pub(super) const SDIO_HOST2_COMPAT_POLL_LIMIT: u32 = 1_000_000;

// SAFETY: The physical host is only accessed through `Host2Shared`, which
// serializes mutable borrows. `command_request` is only touched through
// `&mut self` and is aborted in `Drop`.
unsafe impl<H> Send for SdioHost2Adapter<H>
where
    H: SdioHost2Irq + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
}

// SAFETY: Shared references only expose `with_host`, mediated by
// `Host2Shared`. Request mutation and IRQ endpoint extraction still require
// `&mut self`.
unsafe impl<H> Sync for SdioHost2Adapter<H>
where
    H: SdioHost2Irq + Send + 'static,
    H::TransactionRequest<'static>: Send,
{
}

impl<H: SdioHost2Irq + 'static> Drop for SdioHost2Adapter<H> {
    fn drop(&mut self) {
        let Some(mut request) = self.command_request.take() else {
            return;
        };
        let result = self
            .core
            .with_mut(|host| host.abort_transaction(&mut request))
            .map_err(host2_error);
        if let Err(err) = result {
            warn!(
                "sdio-host2 adapter: abort pending command on drop reported recovery error: \
                 {err:?}"
            );
        }
    }
}

pub struct SdioHost2DataRequest<'a, H: SdioHost2Irq + 'static> {
    core: Host2Shared<H>,
    inner: Option<H::TransactionRequest<'a>>,
    completed_dma: Option<CompletedDma>,
}

impl<H: SdioHost2Irq + 'static> SdioHost2DataRequest<'_, H> {
    pub(crate) fn abort(&mut self) -> Result<(), Error> {
        let Some(mut request) = self.inner.take() else {
            return Ok(());
        };
        let (result, completed_dma) = self.core.with_mut(|host| {
            let result = host.abort_transaction(&mut request).map_err(host2_error);
            let completed_dma = host.take_completed_dma(&mut request);
            (result, completed_dma)
        });
        self.completed_dma = completed_dma;
        result
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn take_completed_dma(&mut self) -> Option<CompletedDma> {
        self.completed_dma.take()
    }
}

impl<H: SdioHost2Irq + 'static> Drop for SdioHost2DataRequest<'_, H> {
    fn drop(&mut self) {
        if let Err(err) = self.abort() {
            warn!(
                "sdio-host2 adapter: abort pending data request on drop reported recovery error: \
                 {err:?}"
            );
        }
    }
}

pub struct SdioHost2BusRequest<H: SdioHost2Irq + 'static> {
    core: Host2Shared<H>,
    inner: Option<H::BusRequest>,
    op: sdio_host2::BusOp,
}

impl<H: SdioHost2Irq + 'static> SdioHost2BusRequest<H> {
    fn abort(&mut self) -> Result<(), Error> {
        let Some(mut request) = self.inner.take() else {
            return Ok(());
        };
        self.core
            .with_mut(|host| host.abort_bus_op(&mut request))
            .map_err(host2_error)
    }
}

impl<H: SdioHost2Irq + 'static> Drop for SdioHost2BusRequest<H> {
    fn drop(&mut self) {
        if let Err(err) = self.abort() {
            warn!(
                "sdio-host2 adapter: abort pending bus op on drop reported recovery error: {err:?}"
            );
        }
    }
}

impl<H: SdioHost2Irq + 'static> SdioHost for SdioHost2Adapter<H> {
    type Event = H::Event;
    type DataRequest<'a>
        = SdioHost2DataRequest<'a, H>
    where
        Self: 'a;
    type BusRequest = SdioHost2BusRequest<H>;

    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error> {
        if self.command_request.is_some() {
            return Err(Error::Busy);
        }
        debug!(
            "sdio-host2 adapter: submit command CMD{} arg={:#010x} resp={:?}",
            cmd.index, cmd.argument, cmd.response
        );
        let request = self
            .core
            .with_mut(|host| unsafe {
                host.submit_transaction(sdio_host2::Transaction::command(*cmd))
            })
            .map_err(host2_error)?;
        self.command_request = Some(request);
        Ok(())
    }

    fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error> {
        let mut request = self.command_request.take().ok_or(Error::InvalidArgument)?;
        match self
            .core
            .with_mut(|host| host.poll_transaction(&mut request))
        {
            Ok(sdio_host2::RequestPoll::Pending) => {
                self.command_request = Some(request);
                Ok(CommandResponsePoll::Pending)
            }
            Ok(sdio_host2::RequestPoll::Ready(Ok(raw))) => {
                crate::response::response_from_raw(raw).map(CommandResponsePoll::Complete)
            }
            Ok(sdio_host2::RequestPoll::Ready(Err(err))) => {
                warn!("sdio-host2 adapter: command completed with error {:?}", err);
                Err(host2_error(err))
            }
            Err(err) => {
                warn!("sdio-host2 adapter: command poll failed with {:?}", err);
                self.command_request = Some(request);
                Err(host2_poll_error(err))
            }
        }
    }

    fn submit_read_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        let data = sdio_host2::DataPhase::read(
            nonzero_block_size(block_size)?,
            nonzero_block_count(block_count)?,
            buf,
        )
        .map_err(host2_error)?;
        let request = self
            .core
            .with_mut(|host| unsafe {
                host.submit_transaction(sdio_host2::Transaction::with_data(*cmd, data))
            })
            .map_err(host2_error)?;
        Ok(SdioHost2DataRequest {
            core: self.core.clone(),
            inner: Some(request),
            completed_dma: None,
        })
    }

    fn submit_write_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error> {
        let data = sdio_host2::DataPhase::write(
            nonzero_block_size(block_size)?,
            nonzero_block_count(block_count)?,
            buf,
        )
        .map_err(host2_error)?;
        let request = self
            .core
            .with_mut(|host| unsafe {
                host.submit_transaction(sdio_host2::Transaction::with_data(*cmd, data))
            })
            .map_err(host2_error)?;
        Ok(SdioHost2DataRequest {
            core: self.core.clone(),
            inner: Some(request),
            completed_dma: None,
        })
    }

    fn poll_data_request<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
    ) -> Result<DataCommandPoll, Error> {
        let inner = request.inner.as_mut().ok_or(Error::InvalidArgument)?;
        match request.core.with_mut(|host| host.poll_transaction(inner)) {
            Ok(sdio_host2::RequestPoll::Pending) => Ok(DataCommandPoll::Pending),
            Ok(sdio_host2::RequestPoll::Ready(Ok(raw))) => {
                request.completed_dma = request
                    .inner
                    .as_mut()
                    .and_then(|inner| request.core.with_mut(|host| host.take_completed_dma(inner)));
                request.inner = None;
                crate::response::response_from_raw(raw).map(DataCommandPoll::Complete)
            }
            Ok(sdio_host2::RequestPoll::Ready(Err(err))) => {
                request.completed_dma = request
                    .inner
                    .as_mut()
                    .and_then(|inner| request.core.with_mut(|host| host.take_completed_dma(inner)));
                request.inner = None;
                Err(host2_error(err))
            }
            Err(err) => Err(host2_poll_error(err)),
        }
    }

    fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error> {
        let mut request = self.submit_bus_op(SdioBusOp::SetBusWidth(width))?;
        self.drain_bus_op(&mut request)
    }

    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error> {
        let mut request = self.submit_bus_op(SdioBusOp::SetClock(speed))?;
        self.drain_bus_op(&mut request)
    }

    fn switch_voltage(&mut self, voltage: SignalVoltage) -> Result<(), Error> {
        let mut request = self.submit_bus_op(SdioBusOp::SwitchVoltage(voltage))?;
        self.drain_bus_op(&mut request)
    }

    fn execute_tuning(&mut self, cmd_index: u8, block_size: NonZeroU16) -> Result<(), Error> {
        let mut request = self.submit_bus_op(SdioBusOp::ExecuteTuning {
            cmd_index,
            block_size,
        })?;
        self.drain_bus_op(&mut request)
    }

    fn submit_bus_op(&mut self, op: SdioBusOp) -> Result<Self::BusRequest, Error> {
        let host_op = match op {
            SdioBusOp::ResetAll => sdio_host2::BusOp::ResetAll,
            SdioBusOp::PowerOn => sdio_host2::BusOp::PowerOn,
            SdioBusOp::PowerOff => sdio_host2::BusOp::PowerOff,
            SdioBusOp::SetBusWidth(width) => sdio_host2::BusOp::SetBusWidth(width),
            SdioBusOp::SetClock(speed) => sdio_host2::BusOp::SetClock(speed),
            SdioBusOp::SwitchVoltage(voltage) => sdio_host2::BusOp::SetSignalVoltage(voltage),
            SdioBusOp::ExecuteTuning {
                cmd_index,
                block_size,
            } => {
                let command = Command::new(cmd_index, 0, ResponseType::R1);
                sdio_host2::BusOp::ExecuteTuning {
                    command,
                    block_size,
                }
            }
        };
        let inner = self
            .core
            .with_mut(|host| unsafe { host.submit_bus_op(host_op) })
            .map_err(host2_error)?;
        Ok(SdioHost2BusRequest {
            core: self.core.clone(),
            inner: Some(inner),
            op: host_op,
        })
    }

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error> {
        let inner = request.inner.as_mut().ok_or(Error::InvalidArgument)?;
        match request.core.with_mut(|host| host.poll_bus_op(inner)) {
            Ok(sdio_host2::RequestPoll::Pending) => Ok(OperationPoll::Pending),
            Ok(sdio_host2::RequestPoll::Ready(Ok(()))) => {
                request.inner = None;
                Ok(OperationPoll::Complete(()))
            }
            Ok(sdio_host2::RequestPoll::Ready(Err(err))) => {
                warn!(
                    "sdio-host2 adapter: bus op {:?} completed with error {:?}",
                    request.op, err
                );
                request.inner = None;
                Err(host2_error(err))
            }
            Err(err) => {
                warn!(
                    "sdio-host2 adapter: bus op {:?} poll failed with {:?}",
                    request.op, err
                );
                Err(host2_poll_error(err))
            }
        }
    }

    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        self.core.with_mut(|host| host.enable_completion_irq())
    }

    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        self.core.with_mut(|host| host.disable_completion_irq())
    }

    fn completion_irq_enabled(&self) -> bool {
        self.core.with_ref(|host| host.completion_irq_enabled())
    }

    fn now_ms(&self) -> Option<u64> {
        self.core.with_ref(|host| host.now_ms())
    }
}

impl<H: SdioHost2Irq + 'static> SdioIrqHost for SdioHost2Adapter<H> {
    type IrqHandle = H::IrqHandle;

    fn irq_handle(&mut self) -> Self::IrqHandle {
        self.core.with_mut(|host| host.irq_handle())
    }
}

#[cfg(feature = "rdif")]
impl<H: SdioHost2Irq + 'static> SdioHost2Adapter<H> {
    pub(crate) fn submit_dma_data(
        &mut self,
        cmd: &Command,
        direction: sdio_host2::DataDirection,
        buffer: PreparedDma,
        block_size: u32,
        block_count: u32,
    ) -> Result<SdioHost2DataRequest<'static, H>, DmaSubmitError> {
        let block_size = match nonzero_block_size(block_size) {
            Ok(block_size) => block_size,
            Err(err) => return Err(DmaSubmitError::new(err, buffer)),
        };
        let block_count = match nonzero_block_count(block_count) {
            Ok(block_count) => block_count,
            Err(err) => return Err(DmaSubmitError::new(err, buffer)),
        };
        let data = sdio_host2::DataPhase::dma(direction, block_size, block_count, buffer).map_err(
            |err| {
                let (error, buffer) = err.into_parts();
                DmaSubmitError::new(host2_error(error), buffer)
            },
        )?;
        let transaction = sdio_host2::Transaction::with_data(*cmd, data);
        let request = self
            .core
            .with_mut(|host| unsafe { host.submit_transaction_owned(transaction) });
        match request {
            Ok(request) => Ok(SdioHost2DataRequest {
                core: self.core.clone(),
                inner: Some(request),
                completed_dma: None,
            }),
            Err(err) => {
                let error = host2_error(err.error);
                let Some(transaction) = err.into_transaction() else {
                    panic!("sdio-host2 DMA submit consumed owned transaction on failure");
                };
                let Some(buffer) = recover_dma_buffer(transaction) else {
                    panic!("sdio-host2 DMA submit failure did not return DMA buffer");
                };
                Err(DmaSubmitError::new(error, buffer))
            }
        }
    }
}

#[cfg(feature = "rdif")]
fn recover_dma_buffer(transaction: sdio_host2::Transaction<'_>) -> Option<PreparedDma> {
    match transaction.data?.buffer {
        sdio_host2::DataBuffer::Dma(buffer) => Some(buffer),
        sdio_host2::DataBuffer::Read(_) | sdio_host2::DataBuffer::Write(_) => None,
    }
}

impl<H: SdioHost2Irq + 'static> SdioSdmmc<SdioHost2Adapter<H>> {
    pub fn new_host2(host: H) -> Self {
        Self::new(SdioHost2Adapter::new(host))
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

fn host2_error(err: sdio_host2::Error) -> Error {
    match err {
        sdio_host2::Error::Busy => Error::Busy,
        sdio_host2::Error::Timeout => Error::Timeout(ErrorContext::default()),
        sdio_host2::Error::Crc => Error::Crc(ErrorContext::default()),
        sdio_host2::Error::NoCard => Error::NoCard,
        sdio_host2::Error::Unsupported => Error::UnsupportedCommand,
        sdio_host2::Error::InvalidArgument => Error::InvalidArgument,
        sdio_host2::Error::Misaligned => Error::Misaligned,
        sdio_host2::Error::Bus => Error::BusError(ErrorContext::default()),
        sdio_host2::Error::Controller => Error::BusError(ErrorContext::default()),
        _ => Error::BusError(ErrorContext::default()),
    }
}

fn host2_poll_error(err: sdio_host2::PollRequestError) -> Error {
    match err {
        sdio_host2::PollRequestError::AlreadyCompleted => Error::InvalidArgument,
        sdio_host2::PollRequestError::WrongOwner
        | sdio_host2::PollRequestError::WrongKind
        | sdio_host2::PollRequestError::StaleGeneration
        | sdio_host2::PollRequestError::RecoveryFailed => Error::BusError(ErrorContext::default()),
        _ => Error::BusError(ErrorContext::default()),
    }
}
