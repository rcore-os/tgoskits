//! Compatibility adapter between `sdio-host2` and the protocol `SdioHost` trait.

#[cfg(feature = "rdif")]
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    num::{NonZeroU16, NonZeroU32},
    sync::atomic::{AtomicBool, Ordering},
};

#[cfg(feature = "rdif")]
use dma_api::PreparedDma;
use dma_api::{CompletedDma, CpuDmaBuffer};
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
    error::{Error, ErrorContext},
    response::ResponseType,
};

#[cfg(feature = "rdif")]
mod recovery;
#[cfg(feature = "rdif")]
use recovery::ErasedRecovery;
#[cfg(feature = "rdif")]
pub use recovery::{SdioHost2Lifecycle, SdioHost2Recovery};

#[cfg(feature = "rdif")]
pub(crate) struct DmaSubmitError {
    pub error: Error,
    buffer: Box<PreparedDma>,
}

#[cfg(feature = "rdif")]
pub(crate) struct CpuSubmitError {
    pub error: Error,
    buffer: Box<CpuDmaBuffer>,
}

#[cfg(feature = "rdif")]
impl CpuSubmitError {
    fn new(error: Error, buffer: CpuDmaBuffer) -> Self {
        Self {
            error,
            buffer: Box::new(buffer),
        }
    }

    pub(crate) fn into_buffer(self) -> CpuDmaBuffer {
        *self.buffer
    }
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
    timed_transactions: TimedTransactions<H>,
    timed_bus_ops: TimedBusOps<H>,
    #[cfg(feature = "rdif")]
    recovery: Option<Box<dyn ErasedRecovery<H>>>,
}

type TimedBusPollFn<H> = fn(
    &mut H,
    &mut <H as sdio_host2::SdioHost>::BusRequest,
    u64,
) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError>;

type TimedTransactionPollFn<H> = for<'a> fn(
    &mut H,
    &mut <H as sdio_host2::SdioHost>::TransactionRequest<'a>,
    u64,
) -> Result<
    sdio_host2::RequestPoll<sdio_host2::RawResponse>,
    sdio_host2::PollRequestError,
>;

struct TimedTransactions<H: SdioHost2Irq + 'static> {
    poll: TimedTransactionPollFn<H>,
    wake_at: for<'a> fn(&H, &H::TransactionRequest<'a>) -> Option<u64>,
}

struct TimedBusOps<H: SdioHost2Irq + 'static> {
    poll: TimedBusPollFn<H>,
    wake_at: fn(&H, &H::BusRequest) -> Option<u64>,
}

/// IRQ-capable extension used by [`SdioHost2Adapter`].
///
/// `sdio-host2` intentionally does not define IRQ abstractions. This protocol
/// crate only needs a way to forward host-specific completion IRQ handles when
/// a physical host is wrapped for the legacy `SdioHost` card state machine.
pub trait SdioHost2Irq: sdio_host2::SdioHost {
    type Event: HostEvent + Default;
    type IrqHandle: SdioIrqHandle<Event = Self::Event>;

    /// Return whether the controller can currently deliver completion IRQs.
    fn completion_irq_enabled(&self) -> bool;

    /// Route command, data, and error completion to the registered IRQ source.
    fn enable_completion_irq(&mut self) -> Result<(), Error>;

    /// Mask completion delivery before the OS drains the IRQ action.
    fn disable_completion_irq(&mut self) -> Result<(), Error>;

    fn irq_handle(&mut self) -> Self::IrqHandle;
}

/// Absolute-time extension for eventless physical-host bus transitions.
///
/// This remains separate from `sdio-host2`'s transaction contract so existing
/// hosts keep their source-compatible polling method. An adapter opts into this
/// capability explicitly with [`SdioHost2Adapter::new_timed`].
pub trait SdioHost2Timed: SdioHost2Irq {
    fn poll_transaction_at<'a>(
        &mut self,
        request: &mut Self::TransactionRequest<'a>,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError>
    where
        Self: 'a;

    fn transaction_wake_at<'a>(&self, request: &Self::TransactionRequest<'a>) -> Option<u64>
    where
        Self: 'a;

    fn poll_bus_op_at(
        &mut self,
        request: &mut Self::BusRequest,
        now_ns: u64,
    ) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError>;

    fn bus_op_wake_at(&self, _request: &Self::BusRequest) -> Option<u64> {
        None
    }
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
            timed_transactions: TimedTransactions {
                poll: poll_legacy_transaction::<H>,
                wake_at: no_transaction_wake::<H>,
            },
            timed_bus_ops: TimedBusOps {
                poll: poll_legacy_bus_op::<H>,
                wake_at: no_bus_op_wake::<H>,
            },
            #[cfg(feature = "rdif")]
            recovery: None,
        }
    }

    /// Wrap a host whose eventless bus operations consume absolute time.
    pub fn new_timed(host: H) -> Self
    where
        H: SdioHost2Timed,
    {
        Self {
            core: Host2Shared::new(host),
            command_request: None,
            timed_transactions: TimedTransactions {
                poll: poll_timed_transaction::<H>,
                wake_at: timed_transaction_wake::<H>,
            },
            timed_bus_ops: TimedBusOps {
                poll: H::poll_bus_op_at,
                wake_at: H::bus_op_wake_at,
            },
            #[cfg(feature = "rdif")]
            recovery: None,
        }
    }

    pub fn with_host<R>(&self, f: impl FnOnce(&H) -> R) -> R {
        self.core.with_ref(f)
    }

    pub fn with_host_mut<R>(&self, f: impl FnOnce(&mut H) -> R) -> R {
        self.core.with_mut(f)
    }
}

fn poll_adapter_data_request<'a, H: SdioHost2Irq + 'static>(
    request: &mut SdioHost2DataRequest<'a, H>,
    poll: impl FnOnce(
        &mut H,
        &mut H::TransactionRequest<'a>,
    ) -> Result<
        sdio_host2::RequestPoll<sdio_host2::RawResponse>,
        sdio_host2::PollRequestError,
    >,
) -> Result<DataCommandPoll, Error> {
    let inner = request.inner.as_mut().ok_or(Error::InvalidArgument)?;
    match request.core.with_mut(|host| poll(host, inner)) {
        Ok(sdio_host2::RequestPoll::Pending) => Ok(DataCommandPoll::Pending),
        Ok(sdio_host2::RequestPoll::Ready(Ok(raw))) => {
            request.completed_dma = request
                .inner
                .as_mut()
                .and_then(|inner| request.core.with_mut(|host| host.take_completed_dma(inner)));
            request.completed_cpu = request
                .inner
                .as_mut()
                .and_then(|inner| request.core.with_mut(|host| host.take_completed_cpu(inner)));
            request.inner = None;
            crate::response::response_from_raw(raw).map(DataCommandPoll::Complete)
        }
        Ok(sdio_host2::RequestPoll::Ready(Err(err))) => {
            let completed_dma = request
                .inner
                .as_mut()
                .and_then(|inner| request.core.with_mut(|host| host.take_completed_dma(inner)));
            let completed_cpu = request
                .inner
                .as_mut()
                .and_then(|inner| request.core.with_mut(|host| host.take_completed_cpu(inner)));
            // A terminal status is not a controller stop proof. Retain the
            // request unless the host returned its owned backing.
            if completed_dma.is_some() || completed_cpu.is_some() {
                request.completed_dma = completed_dma;
                request.completed_cpu = completed_cpu;
                request.inner = None;
            }
            Err(host2_error(err))
        }
        Err(err) => Err(host2_poll_error(err)),
    }
}

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
    completed_cpu: Option<CpuDmaBuffer>,
}

impl<H: SdioHost2Irq + 'static> SdioHost2DataRequest<'_, H> {
    pub(crate) fn abort(&mut self) -> Result<(), Error> {
        let Some(mut request) = self.inner.take() else {
            return Ok(());
        };
        let (result, completed_dma, completed_cpu) = self.core.with_mut(|host| {
            let result = host.abort_transaction(&mut request).map_err(host2_error);
            let completed_dma = host.take_completed_dma(&mut request);
            let completed_cpu = host.take_completed_cpu(&mut request);
            (result, completed_dma, completed_cpu)
        });
        self.completed_dma = completed_dma;
        self.completed_cpu = completed_cpu;
        if result.is_err() && self.completed_dma.is_none() && self.completed_cpu.is_none() {
            // A failed abort is not a terminal ownership transition. Keep the
            // request until controller recovery can prove that its backing is
            // no longer reachable by the engine.
            self.inner = Some(request);
        }
        result
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn take_completed_dma(&mut self) -> Option<CompletedDma> {
        self.completed_dma.take()
    }

    #[cfg(feature = "rdif")]
    pub(crate) fn take_completed_cpu(&mut self) -> Option<CpuDmaBuffer> {
        self.completed_cpu.take()
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
        self.poll_command_response_with(|host, request| host.poll_transaction(request))
    }

    fn poll_command_response_at(&mut self, now_ns: u64) -> Result<CommandResponsePoll, Error> {
        let poll = self.timed_transactions.poll;
        self.poll_command_response_with(|host, request| poll(host, request, now_ns))
    }

    fn command_wake_at(&self) -> Option<u64> {
        let request = self.command_request.as_ref()?;
        let wake_at = self.timed_transactions.wake_at;
        self.core.with_ref(|host| wake_at(host, request))
    }

    fn poll_data_request<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
    ) -> Result<DataCommandPoll, Error> {
        poll_adapter_data_request(request, |host, inner| host.poll_transaction(inner))
    }

    fn poll_data_request_at<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
        now_ns: u64,
    ) -> Result<DataCommandPoll, Error> {
        let poll = self.timed_transactions.poll;
        poll_adapter_data_request(request, |host, inner| poll(host, inner, now_ns))
    }

    fn data_request_wake_at<'a>(&self, request: &Self::DataRequest<'a>) -> Option<u64> {
        let inner = request.inner.as_ref()?;
        let wake_at = self.timed_transactions.wake_at;
        request.core.with_ref(|host| wake_at(host, inner))
    }

    // Command/data submission methods remain below. The timed poll methods
    // above are deliberately separate: submitting transfers ownership, while
    // an absolute activation only permits the next programming transition.

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
            completed_cpu: None,
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
            completed_cpu: None,
        })
    }

    fn set_bus_width(&mut self, _width: BusWidth) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }

    fn set_clock(&mut self, _speed: ClockSpeed) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }

    fn switch_voltage(&mut self, _voltage: SignalVoltage) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }

    fn execute_tuning(&mut self, _cmd_index: u8, _block_size: NonZeroU16) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
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
        let progress = request.core.with_mut(|host| host.poll_bus_op(inner));
        finish_adapter_bus_poll(request, progress)
    }

    fn poll_bus_op_at(
        &mut self,
        request: &mut Self::BusRequest,
        now_ns: u64,
    ) -> Result<OperationPoll<()>, Error> {
        let inner = request.inner.as_mut().ok_or(Error::InvalidArgument)?;
        let poll = self.timed_bus_ops.poll;
        let progress = request.core.with_mut(|host| poll(host, inner, now_ns));
        finish_adapter_bus_poll(request, progress)
    }

    fn bus_op_wake_at(&self, request: &Self::BusRequest) -> Option<u64> {
        let inner = request.inner.as_ref()?;
        let wake_at = self.timed_bus_ops.wake_at;
        request.core.with_ref(|host| wake_at(host, inner))
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

impl<H: SdioHost2Irq + 'static> SdioHost2Adapter<H> {
    fn poll_command_response_with(
        &mut self,
        poll: impl FnOnce(
            &mut H,
            &mut H::TransactionRequest<'static>,
        ) -> Result<
            sdio_host2::RequestPoll<sdio_host2::RawResponse>,
            sdio_host2::PollRequestError,
        >,
    ) -> Result<CommandResponsePoll, Error> {
        let mut request = self.command_request.take().ok_or(Error::InvalidArgument)?;
        match self.core.with_mut(|host| poll(host, &mut request)) {
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
}

fn finish_adapter_bus_poll<H: SdioHost2Irq + 'static>(
    request: &mut SdioHost2BusRequest<H>,
    progress: Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError>,
) -> Result<OperationPoll<()>, Error> {
    match progress {
        Ok(sdio_host2::RequestPoll::Pending) => Ok(OperationPoll::Pending),
        Ok(sdio_host2::RequestPoll::Ready(Ok(()))) => {
            request.inner = None;
            Ok(OperationPoll::Complete(()))
        }
        Ok(sdio_host2::RequestPoll::Ready(Err(error))) => {
            warn!(
                "sdio-host2 adapter: bus op {:?} completed with error {:?}",
                request.op, error
            );
            request.inner = None;
            Err(host2_error(error))
        }
        Err(error) => {
            warn!(
                "sdio-host2 adapter: bus op {:?} poll failed with {:?}",
                request.op, error
            );
            Err(host2_poll_error(error))
        }
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
                completed_cpu: None,
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

    pub(crate) fn submit_cpu_data(
        &mut self,
        cmd: &Command,
        direction: sdio_host2::DataDirection,
        buffer: CpuDmaBuffer,
        block_size: u32,
        block_count: u32,
    ) -> Result<SdioHost2DataRequest<'static, H>, CpuSubmitError> {
        let block_size = match nonzero_block_size(block_size) {
            Ok(block_size) => block_size,
            Err(err) => return Err(CpuSubmitError::new(err, buffer)),
        };
        let block_count = match nonzero_block_count(block_count) {
            Ok(block_count) => block_count,
            Err(err) => return Err(CpuSubmitError::new(err, buffer)),
        };
        let data = sdio_host2::DataPhase::owned_cpu(direction, block_size, block_count, buffer)
            .map_err(|err| {
                let (error, buffer) = err.into_parts();
                CpuSubmitError::new(host2_error(error), buffer)
            })?;
        let transaction = sdio_host2::Transaction::with_data(*cmd, data);
        let request = self
            .core
            .with_mut(|host| unsafe { host.submit_transaction_owned(transaction) });
        match request {
            Ok(request) => Ok(SdioHost2DataRequest {
                core: self.core.clone(),
                inner: Some(request),
                completed_dma: None,
                completed_cpu: None,
            }),
            Err(err) => {
                let error = host2_error(err.error);
                let Some(transaction) = err.into_transaction() else {
                    panic!("sdio-host2 PIO submit consumed owned CPU transaction on failure");
                };
                let Some(buffer) = recover_cpu_buffer(transaction) else {
                    panic!("sdio-host2 PIO submit failure did not return CPU buffer");
                };
                Err(CpuSubmitError::new(error, buffer))
            }
        }
    }
}

#[cfg(feature = "rdif")]
fn recover_dma_buffer(transaction: sdio_host2::Transaction<'_>) -> Option<PreparedDma> {
    match transaction.data?.buffer {
        sdio_host2::DataBuffer::Dma(buffer) => Some(buffer),
        sdio_host2::DataBuffer::Read(_)
        | sdio_host2::DataBuffer::Write(_)
        | sdio_host2::DataBuffer::OwnedCpu(_) => None,
    }
}

#[cfg(feature = "rdif")]
fn recover_cpu_buffer(transaction: sdio_host2::Transaction<'_>) -> Option<CpuDmaBuffer> {
    match transaction.data?.buffer {
        sdio_host2::DataBuffer::OwnedCpu(buffer) => Some(buffer),
        sdio_host2::DataBuffer::Read(_)
        | sdio_host2::DataBuffer::Write(_)
        | sdio_host2::DataBuffer::Dma(_) => None,
    }
}

impl<H: SdioHost2Irq + 'static> SdioSdmmc<SdioHost2Adapter<H>> {
    pub fn new_host2(host: H) -> Self {
        Self::new(SdioHost2Adapter::new(host))
    }

    /// Construct a host2 adapter that propagates absolute bus-operation
    /// activation deadlines into card initialization.
    pub fn new_host2_timed(host: H) -> Self
    where
        H: SdioHost2Timed,
    {
        Self::new(SdioHost2Adapter::new_timed(host))
    }
}

fn poll_legacy_bus_op<H: SdioHost2Irq>(
    host: &mut H,
    request: &mut H::BusRequest,
    _now_ns: u64,
) -> Result<sdio_host2::RequestPoll<()>, sdio_host2::PollRequestError> {
    host.poll_bus_op(request)
}

fn poll_legacy_transaction<'a, H: SdioHost2Irq>(
    host: &mut H,
    request: &mut H::TransactionRequest<'a>,
    _now_ns: u64,
) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError> {
    host.poll_transaction(request)
}

fn poll_timed_transaction<'a, H: SdioHost2Timed + 'static>(
    host: &mut H,
    request: &mut H::TransactionRequest<'a>,
    now_ns: u64,
) -> Result<sdio_host2::RequestPoll<sdio_host2::RawResponse>, sdio_host2::PollRequestError> {
    H::poll_transaction_at(host, request, now_ns)
}

fn timed_transaction_wake<'a, H: SdioHost2Timed + 'static>(
    host: &H,
    request: &H::TransactionRequest<'a>,
) -> Option<u64> {
    H::transaction_wake_at(host, request)
}

fn no_transaction_wake<'a, H: SdioHost2Irq>(
    _host: &H,
    _request: &H::TransactionRequest<'a>,
) -> Option<u64> {
    None
}

fn no_bus_op_wake<H: SdioHost2Irq>(_host: &H, _request: &H::BusRequest) -> Option<u64> {
    None
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
