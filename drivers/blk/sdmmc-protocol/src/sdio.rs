//! SDIO (Secure Digital Input Output) mode transport layer
//!
//! SDIO mode uses a dedicated host controller with 1-bit or 4-bit data bus.
//! Implement [`SdioHost`] for your platform's SDIO peripheral; the host
//! implementation controls command/data progress.

#[cfg(feature = "rdif")]
use alloc::boxed::Box;
use alloc::sync::Arc;
use core::{
    cell::UnsafeCell,
    num::{NonZeroU16, NonZeroU32},
    sync::atomic::{AtomicBool, Ordering},
    task::Waker,
};

use dma_api::CompletedDma;
#[cfg(feature = "rdif")]
use dma_api::PreparedDma;
use log::{debug, info, warn};
pub use sdio_host2::{BusWidth, ClockSpeed, SignalVoltage};

pub use crate::cmd::DataDirection;
use crate::{
    block::{BlockRequestId, CommandResponsePoll, DataCommandPoll, OperationPoll},
    cmd::Command,
    common::block_addr_of,
    error::{Error, ErrorContext, Phase},
    response::{
        CardState, CidResponse, CsdResponse, OcrResponse, Response, ResponseType, SwitchStatus,
    },
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

/// Host IRQ event category returned by portable controller cores.
///
/// Marked `#[non_exhaustive]`: new event categories (e.g. card-detect,
/// re-tuning required) may be added before 1.0; downstream match sites must
/// keep a `_ => ...` arm.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostEventKind {
    /// No runtime action is required.
    #[default]
    None,
    /// A command response is ready.
    CommandComplete,
    /// A data transfer has completed.
    TransferComplete,
    /// Receive-side FIFO or buffer data is ready.
    ReceiveReady,
    /// Transmit-side FIFO or buffer space is ready.
    TransmitReady,
    /// Hardware reported an error condition.
    Error,
    /// Status is pending but has no stable protocol-level category.
    Other,
}

/// Hardware engine affected by a host IRQ event.
///
/// Marked `#[non_exhaustive]` for forward compatibility.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum HostEventSource {
    /// Whole controller or unknown source.
    #[default]
    Controller,
    /// Command engine.
    Command,
    /// Data engine or block queue.
    Data,
}

/// Stable event summary extracted by a host controller IRQ handler.
pub trait HostEvent {
    fn kind(&self) -> HostEventKind;

    fn source(&self) -> HostEventSource {
        HostEventSource::Controller
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        None
    }
}

impl HostEvent for () {
    fn kind(&self) -> HostEventKind {
        HostEventKind::None
    }
}

/// IRQ fast-path handle for a host controller.
///
/// Implementations are intended to be moved into OS IRQ registration code.
/// `handle_irq()` must acknowledge or clear the hardware interrupt source and
/// cache any status that task-side `poll_*` paths need to observe later.
/// It must not complete block requests, copy DMA buffers, or call OS wake/task
/// APIs.
pub trait SdioIrqHandle: Send + 'static {
    type Event: HostEvent + Default;

    fn handle_irq(&mut self) -> Self::Event;
}

/// Optional IRQ-capable extension of [`SdioHost`].
///
/// The normal data path remains the submit/poll methods on [`SdioHost`].
/// IRQ support only gives OS glue an owned top-half endpoint that clears the
/// device-side source and records status for later task-context polling.
pub trait SdioIrqHost: SdioHost {
    type IrqHandle: SdioIrqHandle<Event = Self::Event>;

    fn irq_handle(&mut self) -> Self::IrqHandle;
}

/// Queue identifier used by SD/MMC block adapters.
pub const SDMMC_BLOCK_QUEUE_ID: usize = 0;

/// Convert a host IRQ event into the fixed SD/MMC block queue hint.
///
/// SD/MMC adapters expose one rdif block queue per controller in this
/// workspace, so any non-empty host event is a stable "queue 0 may progress"
/// signal. Request completion still happens only when task context calls
/// `poll_request()`.
pub fn block_queue_ready_from_host_event(event: &impl HostEvent) -> Option<usize> {
    match event.kind() {
        HostEventKind::None => None,
        _ => Some(SDMMC_BLOCK_QUEUE_ID),
    }
}

/// Trait that the platform must implement for the SDIO host controller.
///
/// The driver tracks the published RCA itself, so host implementations no
/// longer need to snoop R6 responses or expose a `rca()` accessor.
pub trait SdioHost {
    /// Host-controller IRQ event type.
    ///
    /// Portable host crates can expose their native event enum here. The
    /// protocol layer does not interpret it; OS glue maps it to runtime wakeups.
    type Event: HostEvent + Default;

    /// Submit a command without waiting for its response.
    fn submit_command(&mut self, cmd: &Command) -> Result<(), Error>;

    /// Advance a submitted command and harvest the response when complete.
    fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error>;

    type DataRequest<'a>
    where
        Self: 'a;

    /// Submit a read-data command without waiting for its data phase.
    fn submit_read_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error>;

    /// Submit a write-data command without waiting for its data phase.
    fn submit_write_data<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<Self::DataRequest<'a>, Error>;

    /// Advance a previously submitted data command without blocking.
    fn poll_data_request<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
    ) -> Result<DataCommandPoll, Error>;

    type BusRequest;

    /// Set the bus width
    fn set_bus_width(&mut self, width: BusWidth) -> Result<(), Error>;

    /// Set the clock speed
    fn set_clock(&mut self, speed: ClockSpeed) -> Result<(), Error>;

    /// Switch the bus signaling voltage (typically 3.3 V → 1.8 V for
    /// UHS-I or HS200 entry). The protocol layer issues CMD11 *before*
    /// calling this; the host is responsible for the controller-side
    /// transition (gate SD clock → flip the IO domain → wait t_VSW
    /// (≥ 5 ms) → re-enable SD clock at the new level → confirm
    /// `DAT[3:0]` is high).
    ///
    /// Default returns `UnsupportedCommand` so hosts that don't implement
    /// 1.8 V signaling get a clean fallback path instead of silently
    /// keeping the bus at 3.3 V.
    fn switch_voltage(&mut self, _voltage: SignalVoltage) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }

    /// Run the controller's tuning state machine for the given command
    /// index (CMD19 for SD UHS-I, CMD21 for eMMC HS200). The host is
    /// responsible for issuing tuning blocks in a loop, comparing
    /// against the expected pattern, and reporting back whether a
    /// stable sampling phase was found. `block_size` is the protocol tuning
    /// pattern length: SD CMD19 is 64 bytes, MMC CMD21 is 64 bytes on 4-bit
    /// buses and 128 bytes on 8-bit buses.
    ///
    /// Default returns `UnsupportedCommand`. Hosts that report success
    /// without actually tuning are silently lying to the caller — only
    /// implement this when the controller can validate the result.
    fn execute_tuning(&mut self, _cmd_index: u8, _block_size: NonZeroU16) -> Result<(), Error> {
        Err(Error::UnsupportedCommand)
    }

    fn submit_bus_op(&mut self, op: SdioBusOp) -> Result<Self::BusRequest, Error>;

    fn poll_bus_op(&mut self, request: &mut Self::BusRequest) -> Result<OperationPoll<()>, Error>;

    /// Route command/data completion and error status to the host IRQ line.
    ///
    /// Default is a no-op so polling-only hosts do not have to implement IRQ
    /// support.
    fn enable_completion_irq(&mut self) -> Result<(), Error> {
        Ok(())
    }

    /// Mask host IRQ delivery while keeping the controller usable for polling.
    ///
    /// Default is a no-op for polling-only hosts.
    fn disable_completion_irq(&mut self) -> Result<(), Error> {
        Ok(())
    }

    fn completion_irq_enabled(&self) -> bool {
        false
    }

    /// Register the task that should be woken when command or data progress is
    /// possible. Polling-only hosts may keep the default no-op implementation.
    fn register_waker(&mut self, _waker: &Waker) {}

    /// Optional monotonic wall-clock source, in milliseconds.
    ///
    /// `None` (the default) means the host has no clock; the protocol layer
    /// falls back to the poll-counter timeouts documented in
    /// [`SdioInitTiming`] / [`MmcSwitchTiming`]. `Some(t)` switches the
    /// ACMD41 / CMD1 power-up and MMC `CMD6 SWITCH` busy-wait budgets to
    /// wall-clock deadlines, making timeouts independent of caller poll
    /// cadence.
    ///
    /// The protocol layer keeps both checks active whenever a clock is
    /// available — whichever fires first surfaces as `Error::Timeout`. So a
    /// host that opts in via this method gets accurate timeouts even when
    /// glue polls very slowly, and is still protected by the poll budget if
    /// the clock unexpectedly stalls.
    ///
    /// Implementations must be monotonic across calls within a single host
    /// instance. Resolution finer than 1 ms is fine but not required —
    /// jiffies at 100 Hz works. Wraparound at `u64` milliseconds
    /// (~584 million years) is safe to ignore.
    fn now_ms(&self) -> Option<u64> {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdioBusOp {
    ResetAll,
    PowerOn,
    PowerOff,
    SetBusWidth(BusWidth),
    SetClock(ClockSpeed),
    SwitchVoltage(SignalVoltage),
    ExecuteTuning {
        cmd_index: u8,
        block_size: NonZeroU16,
    },
}

#[derive(Debug, Clone, Copy)]
pub struct ReadyBusRequest;

pub fn submit_ready_bus_op<H: SdioHost<BusRequest = ReadyBusRequest>>(
    host: &mut H,
    op: SdioBusOp,
) -> Result<ReadyBusRequest, Error> {
    match op {
        SdioBusOp::ResetAll | SdioBusOp::PowerOn | SdioBusOp::PowerOff => {}
        SdioBusOp::SetBusWidth(width) => host.set_bus_width(width)?,
        SdioBusOp::SetClock(speed) => host.set_clock(speed)?,
        SdioBusOp::SwitchVoltage(voltage) => host.switch_voltage(voltage)?,
        SdioBusOp::ExecuteTuning {
            cmd_index,
            block_size,
        } => host.execute_tuning(cmd_index, block_size)?,
    }
    Ok(ReadyBusRequest)
}

pub fn poll_ready_bus_op(_request: &mut ReadyBusRequest) -> Result<OperationPoll<()>, Error> {
    Ok(OperationPoll::Complete(()))
}

/// Compatibility adapter that lets the SD/MMC card state machine run on a
/// physical [`sdio_host2::SdioHost`] implementation.
///
/// The protocol-facing [`SdioHost`] trait is kept for existing callers. New
/// host crates can implement `sdio_host2::SdioHost` natively and pass the host
/// through this adapter.
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

const SDIO_HOST2_COMPAT_POLL_LIMIT: u32 = 1_000_000;

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

/// Poll-cadence contract for [`SdioSdmmc::poll_init_request`] and the
/// [`MmcSwitchRequest`] busy-wait sub-state.
///
/// The protocol layer does not own a wall clock. Every internal "elapsed
/// time" counter (ACMD41 / CMD1 power-up budget, MMC `CMD6 SWITCH` busy-wait
/// budget) increments by exactly one tick per `poll_*` invocation and is
/// compared against [`SdioInitTiming::MAX_POLLS`] / [`MmcSwitchTiming::MAX_POLLS`].
/// That means timeouts are expressed in **poll iterations**, not seconds.
///
/// Caller glue (executor yield, OS sleep, IRQ wakeup) **MUST pace `poll_*`
/// invocations at roughly [`SdioInitTiming::POLL_TICK_MS_HINT`]** — failing
/// to do so does not produce undefined behavior, but the observed timeout
/// will diverge from the documented budget:
///
/// - Tight `loop { poll() }` → ACMD41 1 s budget collapses to microseconds.
/// - Executor that wakes only on hardware IRQ with no fallback timer →
///   budget extends to seconds because the tick counter advances slowly.
///
/// The 10 ms cadence matches the SD spec's recommended ACMD41 retry rate
/// (sect. 4.2.3) and gives ~100 retries before the protocol layer surfaces
/// `Error::Timeout`.
///
/// # Wall-clock escape hatch
///
/// Hosts that implement [`SdioHost::now_ms`] get an additional wall-clock
/// deadline check layered on top of the poll counter:
/// [`SdioInitTiming::TIMEOUT_MS`] / [`MmcSwitchTiming::TIMEOUT_MS`] are
/// enforced against `now_ms() - submit_time_ms`, so the budgets stay
/// accurate no matter how slow or fast the caller polls. The poll counter
/// is still consulted as a fallback that fires if the clock unexpectedly
/// stalls; whichever check trips first surfaces as `Error::Timeout`.
struct SdioInitTiming;

impl SdioInitTiming {
    /// Wall-time the protocol layer **assumes** elapses between two
    /// successive `poll_init_request` invocations. Document-only — the
    /// protocol code itself never multiplies anything by this constant.
    /// Caller glue should pace polls at approximately this cadence.
    const POLL_TICK_MS_HINT: u32 = 10;

    /// Maximum number of `poll_init_request` iterations the protocol layer
    /// will tolerate while waiting for ACMD41 (SD) or CMD1 (MMC) to report
    /// `card_powered_up`. At the [`Self::POLL_TICK_MS_HINT`] cadence this is
    /// equivalent to ~1 second.
    const MAX_POLLS: u32 = 100;

    /// Wall-clock budget for ACMD41 / CMD1 power-up retries, enforced when
    /// the host implements [`SdioHost::now_ms`]. Matches the SD spec's
    /// recommended 1 s ACMD41 retry window (sect. 4.2.3).
    const TIMEOUT_MS: u64 = 1_000;
}

struct MmcSwitchTiming;

impl MmcSwitchTiming {
    /// Maximum number of poll iterations spent waiting for an MMC
    /// `CMD6 SWITCH` to leave the Programming state. At the
    /// [`SdioInitTiming::POLL_TICK_MS_HINT`] cadence this is equivalent to
    /// ~250 ms — long enough to absorb worst-case `GENERIC_CMD6_TIME` while
    /// short enough that a hung card surfaces as `Error::Timeout` rather
    /// than blocking init forever.
    const MAX_POLLS: u32 = 25;

    /// Wall-clock budget for the MMC `CMD6 SWITCH` busy-wait, enforced when
    /// the host implements [`SdioHost::now_ms`]. Sized to match `MAX_POLLS`
    /// at the recommended poll cadence so clock-aware and poll-only hosts
    /// see the same effective budget.
    const TIMEOUT_MS: u64 = 250;
}

/// Return whether the wall-clock budget for ACMD41 / CMD1 power-up has
/// elapsed. `started_ms` is the time the busy-wait phase began (captured
/// from [`SdioHost::now_ms`] on the first not-ready response). The check is
/// a no-op when either the host has no clock or the budget has not been
/// armed yet.
fn power_up_deadline_passed<H: SdioHost>(host: &H, started_ms: Option<u64>) -> bool {
    match (started_ms, host.now_ms()) {
        (Some(started), Some(now)) => now.saturating_sub(started) >= SdioInitTiming::TIMEOUT_MS,
        _ => false,
    }
}

/// Return whether the wall-clock budget for MMC `CMD6 SWITCH` has elapsed.
/// See [`power_up_deadline_passed`] for the contract.
fn mmc_switch_deadline_passed<H: SdioHost>(host: &H, request: &MmcSwitchRequest) -> bool {
    let elapsed_exceeded = match (request.started_ms, host.now_ms()) {
        (Some(started), Some(now)) => now.saturating_sub(started) >= MmcSwitchTiming::TIMEOUT_MS,
        _ => false,
    };
    elapsed_exceeded || request.polls >= MmcSwitchTiming::MAX_POLLS
}

/// SDIO mode SD/MMC driver
pub struct SdioSdmmc<H: SdioHost> {
    host: H,
    rca: u16,
    high_capacity: bool,
    bus_width: BusWidth,
    kind: CardKind,
    sd_speed_selection_enabled: bool,
    sd_uhs_selection_enabled: bool,
}

pub struct SdioDataRequest<'a, H: SdioHost + 'a> {
    inner: H::DataRequest<'a>,
}

/// Submitted SDIO command transaction.
pub struct SdioCommandRequest;

/// Submitted `CMD13 SEND_STATUS` transaction.
pub struct SdioStatusRequest {
    inner: SdioCommandRequest,
}

/// Submitted MMC `CMD8 SEND_EXT_CSD` data transaction.
pub struct ExtCsdRequest<'a, H: SdioHost + 'a> {
    inner: SdioDataRequest<'a, H>,
}

/// Submitted SD `CMD6 SWITCH_FUNC` data transaction.
pub struct SwitchFunctionRequest<'a, H: SdioHost + 'a> {
    inner: SdioDataRequest<'a, H>,
}

/// Submitted MMC `CMD6 SWITCH` transaction.
pub struct MmcSwitchRequest {
    rca: u16,
    index: u8,
    value: u8,
    polls: u32,
    /// Wall-clock submit time captured from [`SdioHost::now_ms`], used as
    /// the start of the [`MmcSwitchTiming::TIMEOUT_MS`] window. `None`
    /// means the host has no clock and only [`MmcSwitchTiming::MAX_POLLS`]
    /// gates the busy-wait.
    started_ms: Option<u64>,
    state: MmcSwitchRequestState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MmcSwitchRequestState {
    PollSwitch,
    PollStatus,
}

/// Card initialization probe order.
///
/// Marked `#[non_exhaustive]`: SDIO-only / no-SD-fallback modes may be added
/// before 1.0; downstream match sites must keep a `_ => ...` arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CardInitPreference {
    /// Probe SD first, then fall back to MMC.
    SdFirst,
    /// Probe SD only. Use this when firmware marks the slot `no-mmc`.
    SdOnly,
    /// Probe MMC first. Use this for controller instances wired to eMMC.
    MmcFirst,
}

impl CardInitPreference {
    fn starts_with_sd(self) -> bool {
        matches!(self, Self::SdFirst | Self::SdOnly)
    }

    fn allows_mmc_fallback(self) -> bool {
        matches!(self, Self::SdFirst)
    }
}

/// Caller-owned scratch buffers for SD/MMC initialization data commands.
///
/// Keeping the buffers on the caller's side keeps the `SdioInitRequest`
/// transferable across `Send` boundaries without pinning, and lets bring-up
/// code reuse the same backing storage across retries.
pub struct SdioInitScratch {
    ext_csd: [u8; 512],
    switch_status: [u8; 64],
}

impl SdioInitScratch {
    pub const fn new() -> Self {
        Self {
            ext_csd: [0; 512],
            switch_status: [0; 64],
        }
    }
}

impl Default for SdioInitScratch {
    fn default() -> Self {
        Self::new()
    }
}

/// Pointer to a fixed-size scratch buffer with runtime borrow tracking.
///
/// The init state machine is *self-referential*: an in-flight data request
/// (`ExtCsdRequest`, `SwitchFunctionRequest`) lends the buffer to the host
/// for the duration of a transfer, and the host's `DataRequest<'a>` type
/// ties that lifetime back to the scratch. Rust's borrow checker can't
/// express "host has the buffer until the next `poll_*` reports Complete"
/// inside `SdioInitRequest`, so the code uses a raw pointer.
///
/// `ScratchSlot` keeps that pointer but adds a debug-time `lent` flag, so
/// any future state-machine path that tries to peek into the buffer while
/// it's still on loan to the host (which would be a use-after-free /
/// aliasing UB on real hardware) trips an assertion in development builds.
/// In release builds the flag is still tracked but the assertions compile
/// down to nothing, preserving the zero-overhead intent.
///
/// # Safety
///
/// Constructing a `ScratchSlot` is safe: the constructor takes a `&'a mut`
/// reference and the surrounding [`SdioInitRequest`] carries `'a` so the
/// underlying storage cannot be dropped while the slot is reachable. The
/// pointer-based accessors (`lend`, `peek`) are `unsafe` to call when the
/// borrow state lies (i.e. you returned the buffer to the host without
/// calling `release`); the `_ = lend(); release()` discipline below makes
/// that hard to get wrong.
struct ScratchSlot<const N: usize> {
    ptr: core::ptr::NonNull<[u8; N]>,
    lent: bool,
}

impl<const N: usize> ScratchSlot<N> {
    fn new(buf: &mut [u8; N]) -> Self {
        Self {
            ptr: core::ptr::NonNull::from(buf),
            lent: false,
        }
    }

    /// Hand the buffer to a data-engine call site. Records that the buffer
    /// is on loan; pair with [`Self::release`] once the request completes.
    ///
    /// # Safety
    ///
    /// The returned `&mut [u8; N]` is aliased with the raw pointer held by
    /// this slot. Caller must ensure no other path reads through the slot
    /// (via `peek` / `lend`) until [`Self::release`] is called. The init
    /// state machine satisfies this by gating all access on
    /// `request.{ext_csd,switch_function}_request.is_some()`.
    unsafe fn lend<'b>(&mut self) -> &'b mut [u8; N] {
        debug_assert!(
            !self.lent,
            "scratch slot lent twice without release; this is a state-machine bug"
        );
        self.lent = true;
        unsafe { &mut *self.ptr.as_ptr() }
    }

    /// Mark the buffer as no longer owned by the host so `peek` is safe.
    /// Idempotent.
    fn release(&mut self) {
        self.lent = false;
    }

    /// Read-only view, valid after the host has released the buffer.
    ///
    /// # Safety
    ///
    /// Caller must call this only when the buffer is not on loan to a host
    /// data engine. The `debug_assert!` traps the bug in dev builds.
    unsafe fn peek<'b>(&self) -> &'b [u8; N] {
        debug_assert!(
            !self.lent,
            "scratch slot peeked while still lent to host; this is a state-machine bug"
        );
        unsafe { &*self.ptr.as_ptr() }
    }
}

/// Submitted SDIO initialization transaction.
pub struct SdioInitRequest<'a, H: SdioHost + 'a> {
    state: SdioInitState,
    preference: CardInitPreference,
    sd_v2: bool,
    kind: Option<CardKind>,
    ocr: Option<OcrResponse>,
    cid: Option<CidResponse>,
    capacity_blocks: Option<u64>,
    parsed_ext_csd: Option<crate::ext_csd::ExtCsd>,
    acmd41_polls: u32,
    mmc_polls: u32,
    /// Wall-clock time captured the first time ACMD41 reported the SD card
    /// was not yet powered up. Used together with
    /// [`SdioInitTiming::TIMEOUT_MS`] to surface an accurate timeout when
    /// the host implements [`SdioHost::now_ms`].
    acmd41_started_ms: Option<u64>,
    /// MMC counterpart to `acmd41_started_ms`, captured on the first CMD1
    /// not-ready response.
    mmc_started_ms: Option<u64>,
    mmc_ocr_arg: u32,
    needs_pace: bool,
    ext_csd_buf: ScratchSlot<512>,
    switch_status_buf: ScratchSlot<64>,
    ext_csd_request: Option<ExtCsdRequest<'a, H>>,
    switch_function_request: Option<SwitchFunctionRequest<'a, H>>,
    mmc_switch_request: Option<MmcSwitchRequest>,
    status_request: Option<SdioStatusRequest>,
    command_request: Option<SdioCommandRequest>,
    bus_request: Option<H::BusRequest>,
    active_bus_op: Option<SdioBusOp>,
    current_bus_width: BusWidth,
    current_access_mode: Option<SdAccessMode>,
    sd_access_index: usize,
    mmc_hs200_attempted: bool,
    _scratch: core::marker::PhantomData<&'a mut SdioInitScratch>,
}

impl<'a, H: SdioHost + 'a> SdioInitRequest<'a, H> {
    fn new(preference: CardInitPreference, scratch: &'a mut SdioInitScratch) -> Self {
        Self {
            state: SdioInitState::ResetHost,
            preference,
            sd_v2: false,
            kind: None,
            ocr: None,
            cid: None,
            capacity_blocks: None,
            parsed_ext_csd: None,
            acmd41_polls: 0,
            mmc_polls: 0,
            acmd41_started_ms: None,
            mmc_started_ms: None,
            mmc_ocr_arg: 0,
            needs_pace: false,
            ext_csd_buf: ScratchSlot::new(&mut scratch.ext_csd),
            switch_status_buf: ScratchSlot::new(&mut scratch.switch_status),
            ext_csd_request: None,
            switch_function_request: None,
            mmc_switch_request: None,
            status_request: None,
            command_request: None,
            bus_request: None,
            active_bus_op: None,
            current_bus_width: BusWidth::Bit1,
            current_access_mode: None,
            sd_access_index: 0,
            mmc_hs200_attempted: false,
            _scratch: core::marker::PhantomData,
        }
    }

    /// Consume the pending power-up pacing hint for blocking runtimes.
    ///
    /// `poll_init_request` sets this when the card answered ACMD41/CMD1 but
    /// has not completed power-up yet. Runtime glue can translate it into a
    /// sleep, yield, timer wait, or busy wait. Ordinary command/data pending
    /// states do not set this hint.
    pub fn take_needs_pace(&mut self) -> bool {
        let needs_pace = self.needs_pace;
        self.needs_pace = false;
        needs_pace
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SdioInitState {
    ResetHost,
    PollResetHost,
    PowerOn,
    PollPowerOn,
    ResetVoltage,
    PollResetVoltage,
    ResetBusWidth,
    ResetClock,
    PostIdentificationClockDelay,
    SubmitCmd0,
    PollCmd0,
    PollCmd8,
    PollAcmd41Cmd55,
    PollAcmd41,
    PollMmcInitial,
    PollMmcReady,
    PollCmd2,
    PollCmd3,
    PollCmd9,
    PollCmd7,
    PollSdBusWidthCmd55,
    PollSdBusWidthAcmd6,
    PollSdHostBusWidth,
    FinishCardSetup,
    PollSdDefaultClock,
    PollMmcExtCsd,
    PollMmcBusWidth,
    PollMmcHostBusWidth,
    PrepareMmcSpeed,
    PollMmcHs200VoltageSwitch,
    PollMmcHs200Switch,
    PollMmcHs200Clock,
    PollMmcHs200Tuning,
    PollMmcHs200Status,
    PollMmcHs52Switch,
    PollMmcHighSpeedClock,
    PrepareSdSpeed,
    PollSdSwitchFunctionCheck,
    PollSdVoltageSwitch,
    PollSdSignalVoltage,
    PollSdSetAccessMode,
    PollSdClock,
    PollSdTuning,
    PollSdStatus,
    Complete,
}

#[derive(Debug, Clone, Copy)]
enum SdAccessMode {
    HighSpeed,
    Sdr50,
    Sdr104,
    Ddr50,
}

impl SdAccessMode {
    fn function(self) -> u8 {
        match self {
            Self::HighSpeed => 1,
            Self::Sdr50 => 2,
            Self::Sdr104 => 3,
            Self::Ddr50 => 4,
        }
    }

    fn clock(self) -> ClockSpeed {
        match self {
            Self::HighSpeed => ClockSpeed::HighSpeed,
            Self::Sdr50 => ClockSpeed::Sdr50,
            Self::Sdr104 => ClockSpeed::Sdr104,
            Self::Ddr50 => ClockSpeed::Ddr50,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::HighSpeed => "HighSpeed",
            Self::Sdr50 => "SDR50",
            Self::Sdr104 => "SDR104",
            Self::Ddr50 => "DDR50",
        }
    }
}

impl<H: SdioHost> SdioSdmmc<H> {
    pub fn new(host: H) -> Self {
        Self {
            host,
            rca: 0,
            high_capacity: false,
            bus_width: BusWidth::Bit1,
            kind: CardKind::Sd,
            sd_speed_selection_enabled: true,
            sd_uhs_selection_enabled: true,
        }
    }

    /// Returns mutable access to the underlying SDIO host controller.
    pub fn host_mut(&mut self) -> &mut H {
        &mut self.host
    }

    /// Returns shared access to the underlying SDIO host controller.
    pub fn host(&self) -> &H {
        &self.host
    }

    /// Returns whether the initialized card uses sector addressing.
    pub fn is_high_capacity(&self) -> bool {
        self.high_capacity
    }

    /// Enable or disable optional SD CMD6 speed-mode selection.
    ///
    /// When disabled, SD cards still leave identification mode and run at
    /// default speed, but the driver does not switch the card to HighSpeed or
    /// UHS-I timing.
    pub fn set_sd_speed_selection_enabled(&mut self, enabled: bool) {
        self.sd_speed_selection_enabled = enabled;
    }

    /// Enable or disable UHS-I SD access-mode selection.
    ///
    /// When disabled while SD speed selection remains enabled, initialization
    /// still uses CMD6 to select legacy HighSpeed when the card supports it,
    /// but it does not try CMD11 voltage switching, SDR50, SDR104, DDR50, or
    /// tuning.
    pub fn set_sd_uhs_selection_enabled(&mut self, enabled: bool) {
        self.sd_uhs_selection_enabled = enabled;
    }

    fn mmc_tuning_block_size(&self) -> Result<NonZeroU16, Error> {
        let bytes = if matches!(self.bus_width, BusWidth::Bit8) {
            crate::cmd::MMC_TUNING_BLOCK_SIZE_8BIT
        } else {
            crate::cmd::SD_TUNING_BLOCK_SIZE
        };
        nonzero_block_size(bytes)
    }

    fn sd_tuning_block_size(&self) -> Result<NonZeroU16, Error> {
        nonzero_block_size(crate::cmd::SD_TUNING_BLOCK_SIZE)
    }

    /// Which card family the driver detected. Meaningful only after a
    /// successful [`init`](Self::init); defaults to [`CardKind::Sd`].
    pub fn kind(&self) -> CardKind {
        self.kind
    }

    /// Currently published Relative Card Address. `0` until [`init`](Self::init)
    /// has run successfully.
    pub fn rca(&self) -> u16 {
        self.rca
    }

    pub fn submit_read_blocks_into<'a>(
        &mut self,
        addr: u32,
        buf: &'a mut [u8],
    ) -> Result<SdioDataRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let count = block_count_from_len(buf.len())?;
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = if count == 1 {
            crate::cmd::cmd17(block_addr)
        } else {
            crate::cmd::cmd18(block_addr)
        };
        let inner = self.host.submit_read_data(&cmd, buf, 512, count)?;
        Ok(SdioDataRequest { inner })
    }

    pub fn submit_write_blocks_from<'a>(
        &mut self,
        addr: u32,
        buf: &'a [u8],
    ) -> Result<SdioDataRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let count = block_count_from_len(buf.len())?;
        let block_addr = block_addr_of(addr, self.high_capacity);
        let cmd = if count == 1 {
            crate::cmd::cmd24(block_addr)
        } else {
            crate::cmd::cmd25(block_addr)
        };
        let inner = self.host.submit_write_data(&cmd, buf, 512, count)?;
        Ok(SdioDataRequest { inner })
    }

    pub fn poll_data_request<'a>(
        &mut self,
        request: &mut SdioDataRequest<'a, H>,
    ) -> Result<DataCommandPoll, Error>
    where
        H: 'a,
    {
        self.host.poll_data_request(&mut request.inner)
    }

    pub fn submit_command_request(&mut self, cmd: &Command) -> Result<SdioCommandRequest, Error> {
        self.host.submit_command(cmd)?;
        Ok(SdioCommandRequest)
    }

    pub fn poll_command_request(
        &mut self,
        _request: &mut SdioCommandRequest,
    ) -> Result<CommandResponsePoll, Error> {
        self.host.poll_command_response()
    }

    pub fn submit_status(&mut self) -> Result<SdioStatusRequest, Error> {
        let cmd = crate::cmd::cmd13(self.rca);
        let inner = self.submit_command_request(&cmd)?;
        Ok(SdioStatusRequest { inner })
    }

    pub fn poll_status_request(
        &mut self,
        request: &mut SdioStatusRequest,
    ) -> Result<OperationPoll<CardState>, Error> {
        match self.poll_command_request(&mut request.inner)? {
            CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
            CommandResponsePoll::Complete(Response::R1(r1)) => {
                Ok(OperationPoll::Complete(r1.current_state()))
            }
            CommandResponsePoll::Complete(_) => Err(Error::BadResponse(ErrorContext::for_cmd(
                Phase::ResponseWait,
                13,
            ))),
        }
    }

    pub fn submit_read_data_command<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8],
        block_size: u32,
        block_count: u32,
    ) -> Result<SdioDataRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let inner = self
            .host
            .submit_read_data(cmd, buf, block_size, block_count)?;
        Ok(SdioDataRequest { inner })
    }

    pub fn submit_read_ext_csd<'a>(
        &mut self,
        buf: &'a mut [u8; 512],
    ) -> Result<ExtCsdRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let inner = self.submit_read_data_command(&crate::cmd::CMD8_MMC, buf, 512, 1)?;
        Ok(ExtCsdRequest { inner })
    }

    pub fn poll_ext_csd_request<'a>(
        &mut self,
        request: &mut ExtCsdRequest<'a, H>,
    ) -> Result<OperationPoll<()>, Error>
    where
        H: 'a,
    {
        match self.poll_data_request(&mut request.inner)? {
            DataCommandPoll::Pending => Ok(OperationPoll::Pending),
            DataCommandPoll::Complete(_) => Ok(OperationPoll::Complete(())),
        }
    }

    pub fn submit_switch_function<'a>(
        &mut self,
        cmd: &Command,
        buf: &'a mut [u8; 64],
    ) -> Result<SwitchFunctionRequest<'a, H>, Error>
    where
        H: 'a,
    {
        let inner = self.submit_read_data_command(cmd, buf, 64, 1)?;
        Ok(SwitchFunctionRequest { inner })
    }

    pub fn poll_switch_function_request<'a>(
        &mut self,
        request: &mut SwitchFunctionRequest<'a, H>,
    ) -> Result<OperationPoll<()>, Error>
    where
        H: 'a,
    {
        match self.poll_data_request(&mut request.inner)? {
            DataCommandPoll::Pending => Ok(OperationPoll::Pending),
            DataCommandPoll::Complete(_) => Ok(OperationPoll::Complete(())),
        }
    }

    pub fn submit_mmc_switch(
        &mut self,
        access: u8,
        index: u8,
        value: u8,
    ) -> Result<MmcSwitchRequest, Error> {
        let cmd = crate::cmd::cmd6_mmc_switch(access, index, value);
        let started_ms = self.host.now_ms();
        self.host.submit_command(&cmd)?;
        Ok(MmcSwitchRequest {
            rca: self.rca,
            index,
            value,
            polls: 0,
            started_ms,
            state: MmcSwitchRequestState::PollSwitch,
        })
    }

    pub fn poll_mmc_switch_request(
        &mut self,
        request: &mut MmcSwitchRequest,
    ) -> Result<OperationPoll<()>, Error> {
        match request.state {
            MmcSwitchRequestState::PollSwitch => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    let cmd = crate::cmd::cmd13(request.rca);
                    self.host.submit_command(&cmd)?;
                    request.state = MmcSwitchRequestState::PollStatus;
                    Ok(OperationPoll::Pending)
                }
            },
            MmcSwitchRequestState::PollStatus => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(Response::R1(r1)) => {
                    if r1.switch_error() {
                        warn!(
                            "sdio: SWITCH_ERROR after CMD6 idx={} val={}",
                            request.index, request.value
                        );
                        return Err(Error::CardError(crate::error::CardError::IllegalCommand));
                    }
                    if r1.ready_for_data() && matches!(r1.current_state(), CardState::Transfer) {
                        return Ok(OperationPoll::Complete(()));
                    }
                    if mmc_switch_deadline_passed(&self.host, request) {
                        return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 6)));
                    }
                    request.polls = request.polls.saturating_add(1);
                    let cmd = crate::cmd::cmd13(request.rca);
                    self.host.submit_command(&cmd)?;
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    if mmc_switch_deadline_passed(&self.host, request) {
                        return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 6)));
                    }
                    request.polls = request.polls.saturating_add(1);
                    let cmd = crate::cmd::cmd13(request.rca);
                    self.host.submit_command(&cmd)?;
                    Ok(OperationPoll::Pending)
                }
            },
        }
    }
}

impl<H: SdioHost> SdioSdmmc<H> {
    /// Submit SD/MMC card initialization without waiting for completion.
    ///
    /// # Poll-cadence contract
    ///
    /// The returned [`SdioInitRequest`] expects the caller to drive it via
    /// repeated [`Self::poll_init_request`] calls. The protocol layer does
    /// not own a clock — its ACMD41 / CMD1 timeouts and MMC `CMD6 SWITCH`
    /// busy-waits count poll iterations, not wall-time. Caller glue
    /// (executor yield, OS sleep, IRQ wakeup) **must space `poll_*`
    /// invocations by approximately
    /// [`SdioInitTiming::POLL_TICK_MS_HINT`] (10 ms)**. See
    /// [`SdioInitTiming`] / [`MmcSwitchTiming`] for the full contract.
    /// `take_needs_pace` on the returned request reports when the protocol
    /// layer specifically wants the caller to wait before re-polling
    /// (used during ACMD41 power-up); ordinary pending states do not set
    /// it but still benefit from the same cadence.
    pub fn submit_init<'a>(
        &mut self,
        scratch: &'a mut SdioInitScratch,
    ) -> Result<SdioInitRequest<'a, H>, Error>
    where
        H: 'a,
    {
        self.submit_init_with_preference(CardInitPreference::SdFirst, scratch)
    }

    /// Submit SD/MMC card initialization with a caller-selected probe order.
    pub fn submit_init_with_preference<'a>(
        &mut self,
        preference: CardInitPreference,
        scratch: &'a mut SdioInitScratch,
    ) -> Result<SdioInitRequest<'a, H>, Error>
    where
        H: 'a,
    {
        debug!("sdio: init starting");
        Ok(SdioInitRequest::new(preference, scratch))
    }

    fn submit_init_bus_op<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        op: SdioBusOp,
        next: SdioInitState,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        info!("sdio: submit bus op {:?}", op);
        request.bus_request = Some(self.host.submit_bus_op(op)?);
        request.active_bus_op = Some(op);
        request.state = next;
        Ok(OperationPoll::Pending)
    }

    fn poll_init_bus_op<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<()>, Error> {
        let mut bus_request = request.bus_request.take().ok_or(Error::InvalidArgument)?;
        match self.host.poll_bus_op(&mut bus_request) {
            Ok(OperationPoll::Pending) => {
                request.bus_request = Some(bus_request);
                Ok(OperationPoll::Pending)
            }
            Ok(OperationPoll::Complete(())) => {
                request.active_bus_op = None;
                Ok(OperationPoll::Complete(()))
            }
            Err(err) => {
                warn!(
                    "sdio: init bus op {:?} failed in state {:?}: {:?}",
                    request.active_bus_op, request.state, err
                );
                request.active_bus_op = None;
                Err(err)
            }
        }
    }

    fn submit_init_bus_op_direct<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        op: SdioBusOp,
        next: SdioInitState,
    ) -> Result<(), Error> {
        info!("sdio: submit bus op {:?}", op);
        request.bus_request = Some(self.host.submit_bus_op(op)?);
        request.active_bus_op = Some(op);
        request.state = next;
        Ok(())
    }

    fn poll_init_bus_op_then<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        complete: impl FnOnce(
            &mut Self,
            &mut SdioInitRequest<'a, H>,
        ) -> Result<OperationPoll<CardInfo>, Error>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match self.poll_init_bus_op(request)? {
            OperationPoll::Pending => Ok(OperationPoll::Pending),
            OperationPoll::Complete(()) => complete(self, request),
        }
    }

    /// Advance a submitted initialization request without blocking.
    ///
    /// On any terminal `Err` the controller is reset back toward an
    /// identification-mode-compatible state (1-bit bus, 400 kHz clock, 3.3 V
    /// signaling) so a retry from a fresh [`submit_init`](Self::submit_init)
    /// starts from a known baseline. `Ok(OperationPoll::Pending)` does not
    /// trigger the reset; only terminal failures do.
    pub fn poll_init_request<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match self.poll_init_inner(request) {
            Ok(progress) => Ok(progress),
            Err(err) => {
                warn!("sdio: init aborted ({:?}), resetting host", err);
                self.abort_init();
                Err(err)
            }
        }
    }

    fn poll_init_inner<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        const MMC_HCS: u32 = 1 << 30;
        const MMC_VOLTAGE_MASK: u32 = 0x00FF_8000;
        const MMC_ACCESS_MODE_MASK: u32 = 0x6000_0000;

        match request.state {
            SdioInitState::ResetHost => {
                match self.submit_init_bus_op_direct(
                    request,
                    SdioBusOp::ResetAll,
                    SdioInitState::PollResetHost,
                ) {
                    Ok(()) => {}
                    Err(Error::UnsupportedCommand) => {
                        debug!("sdio: host does not support reset bus op");
                        request.state = SdioInitState::PowerOn;
                    }
                    Err(err) => return Err(err),
                }
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollResetHost => match self.poll_init_bus_op(request)? {
                OperationPoll::Pending => Ok(OperationPoll::Pending),
                OperationPoll::Complete(()) => {
                    request.state = SdioInitState::PowerOn;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PowerOn => {
                match self.submit_init_bus_op_direct(
                    request,
                    SdioBusOp::PowerOn,
                    SdioInitState::PollPowerOn,
                ) {
                    Ok(()) => {}
                    Err(Error::UnsupportedCommand) => {
                        debug!("sdio: host does not support power-on bus op");
                        request.state = SdioInitState::ResetVoltage;
                    }
                    Err(err) => return Err(err),
                }
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollPowerOn => match self.poll_init_bus_op(request)? {
                OperationPoll::Pending => Ok(OperationPoll::Pending),
                OperationPoll::Complete(()) => {
                    request.state = SdioInitState::ResetVoltage;
                    request.needs_pace = true;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::ResetVoltage => {
                match self.submit_init_bus_op_direct(
                    request,
                    SdioBusOp::SwitchVoltage(SignalVoltage::V330),
                    SdioInitState::PollResetVoltage,
                ) {
                    Ok(()) => {}
                    Err(Error::UnsupportedCommand) => {
                        debug!("sdio: host does not support voltage reset");
                        request.state = SdioInitState::ResetBusWidth;
                    }
                    Err(err) => return Err(err),
                }
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollResetVoltage => match self.poll_init_bus_op(request)? {
                OperationPoll::Pending => Ok(OperationPoll::Pending),
                OperationPoll::Complete(()) => self.submit_init_bus_op(
                    request,
                    SdioBusOp::SetBusWidth(BusWidth::Bit1),
                    SdioInitState::ResetClock,
                ),
            },
            SdioInitState::ResetBusWidth => self.submit_init_bus_op(
                request,
                SdioBusOp::SetBusWidth(BusWidth::Bit1),
                SdioInitState::ResetClock,
            ),
            SdioInitState::ResetClock => self.poll_init_bus_op_then(request, |driver, request| {
                driver.submit_init_bus_op(
                    request,
                    SdioBusOp::SetClock(ClockSpeed::Identification),
                    SdioInitState::SubmitCmd0,
                )
            }),
            SdioInitState::SubmitCmd0 => self.poll_init_bus_op_then(request, |_driver, request| {
                request.state = SdioInitState::PostIdentificationClockDelay;
                request.needs_pace = true;
                Ok(OperationPoll::Pending)
            }),
            SdioInitState::PostIdentificationClockDelay => {
                debug!("sdio: CMD0 reset");
                self.host.submit_command(&crate::cmd::CMD0)?;
                request.state = SdioInitState::PollCmd0;
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollCmd0 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    if request.preference.starts_with_sd() {
                        let cmd = crate::cmd::cmd8(0x01, 0xAA);
                        self.host.submit_command(&cmd)?;
                        request.state = SdioInitState::PollCmd8;
                    } else {
                        debug!("sdio: MMC-first init, trying CMD1");
                        self.host.submit_command(&crate::cmd::cmd1(0))?;
                        request.state = SdioInitState::PollMmcInitial;
                    }
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd8 => match self.host.poll_command_response() {
                Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                Ok(CommandResponsePoll::Complete(Response::R7(resp))) => {
                    request.sd_v2 = resp.verify(0x01, 0xAA);
                    debug!("sdio: CMD8 sd_v2={}", request.sd_v2);
                    let cmd55 = crate::cmd::cmd55(0);
                    self.host.submit_command(&cmd55)?;
                    request.state = SdioInitState::PollAcmd41Cmd55;
                    Ok(OperationPoll::Pending)
                }
                Ok(CommandResponsePoll::Complete(_))
                | Err(Error::Timeout(_))
                | Err(Error::BadResponse(_))
                | Err(Error::Crc(_)) => {
                    request.sd_v2 = false;
                    debug!("sdio: CMD8 sd_v2=false");
                    let cmd55 = crate::cmd::cmd55(0);
                    self.host.submit_command(&cmd55)?;
                    request.state = SdioInitState::PollAcmd41Cmd55;
                    Ok(OperationPoll::Pending)
                }
                Err(e) => Err(e),
            },
            SdioInitState::PollAcmd41Cmd55 => match self.host.poll_command_response() {
                Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                Ok(CommandResponsePoll::Complete(_)) => {
                    let acmd41 = crate::cmd::cmd41_with_s18r(request.sd_v2, 0xFF8000, true);
                    self.host.submit_command(&acmd41)?;
                    request.state = SdioInitState::PollAcmd41;
                    Ok(OperationPoll::Pending)
                }
                Err(_sd_err) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(_sd_err);
                    }
                    debug!(
                        "sdio: ACMD41 prologue failed ({:?}), trying MMC CMD1",
                        _sd_err
                    );
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollAcmd41 => match self.host.poll_command_response() {
                Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                Ok(CommandResponsePoll::Complete(Response::R3(ocr))) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Sd);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Sd;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Sd, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdioInitState::PollCmd2;
                    } else {
                        let elapsed_exceeded =
                            power_up_deadline_passed(&self.host, request.acmd41_started_ms);
                        if request.acmd41_polls >= SdioInitTiming::MAX_POLLS || elapsed_exceeded {
                            if !request.preference.allows_mmc_fallback() {
                                return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 41)));
                            }
                            warn!(
                                "sdio: ACMD41 timed out after {} polls (~{} ms at the recommended \
                                 cadence), trying MMC CMD1",
                                request.acmd41_polls,
                                request.acmd41_polls * SdioInitTiming::POLL_TICK_MS_HINT,
                            );
                            self.host.submit_command(&crate::cmd::cmd1(0))?;
                            request.state = SdioInitState::PollMmcInitial;
                            return Ok(OperationPoll::Pending);
                        }
                        if request.acmd41_started_ms.is_none() {
                            request.acmd41_started_ms = self.host.now_ms();
                        }
                        request.acmd41_polls = request.acmd41_polls.saturating_add(1);
                        let cmd55 = crate::cmd::cmd55(0);
                        self.host.submit_command(&cmd55)?;
                        request.state = SdioInitState::PollAcmd41Cmd55;
                        request.needs_pace = true;
                    }
                    Ok(OperationPoll::Pending)
                }
                Ok(CommandResponsePoll::Complete(_)) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 41)));
                    }
                    debug!("sdio: ACMD41 returned bad response, trying MMC CMD1");
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
                Err(_sd_err) => {
                    if !request.preference.allows_mmc_fallback() {
                        return Err(_sd_err);
                    }
                    debug!("sdio: ACMD41 failed ({:?}), trying MMC CMD1", _sd_err);
                    self.host.submit_command(&crate::cmd::cmd1(0))?;
                    request.state = SdioInitState::PollMmcInitial;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollMmcInitial => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(Response::R3(ocr)) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Mmc);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Mmc;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Mmc, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdioInitState::PollCmd2;
                    } else {
                        let voltage = ocr.raw & MMC_VOLTAGE_MASK;
                        let voltage = if voltage == 0 {
                            MMC_VOLTAGE_MASK
                        } else {
                            voltage
                        };
                        request.mmc_ocr_arg = MMC_HCS | voltage | (ocr.raw & MMC_ACCESS_MODE_MASK);
                        let cmd = crate::cmd::cmd1(request.mmc_ocr_arg);
                        self.host.submit_command(&cmd)?;
                        request.state = SdioInitState::PollMmcReady;
                    }
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdioInitState::PollMmcReady => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(Response::R3(ocr)) => {
                    if ocr.card_powered_up() {
                        request.kind = Some(CardKind::Mmc);
                        request.ocr = Some(ocr);
                        self.kind = CardKind::Mmc;
                        info!("sdio: detected {:?} ocr={:#010x}", CardKind::Mmc, ocr.raw);
                        self.host.submit_command(&crate::cmd::CMD2)?;
                        request.state = SdioInitState::PollCmd2;
                    } else {
                        let elapsed_exceeded =
                            power_up_deadline_passed(&self.host, request.mmc_started_ms);
                        if request.mmc_polls >= SdioInitTiming::MAX_POLLS || elapsed_exceeded {
                            warn!(
                                "sdio: CMD1 timed out after {} polls (~{} ms at the recommended \
                                 cadence)",
                                request.mmc_polls,
                                request.mmc_polls * SdioInitTiming::POLL_TICK_MS_HINT,
                            );
                            return Err(Error::Timeout(ErrorContext::for_cmd(Phase::Init, 1)));
                        }
                        if request.mmc_started_ms.is_none() {
                            request.mmc_started_ms = self.host.now_ms();
                        }
                        request.mmc_polls = request.mmc_polls.saturating_add(1);
                        let cmd = crate::cmd::cmd1(request.mmc_ocr_arg);
                        self.host.submit_command(&cmd)?;
                        request.needs_pace = true;
                    }
                    Ok(OperationPoll::Pending)
                }
                CommandResponsePoll::Complete(_) => {
                    Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 1)))
                }
            },
            SdioInitState::PollCmd2 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(response) => {
                    if let Response::R2(raw) = response {
                        request.cid = Some(CidResponse::from_raw(raw));
                    } else {
                        request.cid = None;
                    }
                    match request.kind.ok_or(Error::InvalidArgument)? {
                        CardKind::Sd => self.host.submit_command(&crate::cmd::CMD3_SD)?,
                        CardKind::Mmc => self.host.submit_command(&crate::cmd::cmd3_mmc(1))?,
                    }
                    request.state = SdioInitState::PollCmd3;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd3 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(response) => {
                    self.rca = match (request.kind.ok_or(Error::InvalidArgument)?, response) {
                        (CardKind::Sd, Response::R6(resp)) => resp.rca(),
                        (CardKind::Mmc, Response::R1(_)) => 1,
                        _ => {
                            return Err(Error::BadResponse(ErrorContext::for_cmd(Phase::Init, 3)));
                        }
                    };
                    debug!("sdio: CMD3 rca={:#x}", self.rca);
                    let cmd9 = crate::cmd::cmd9(self.rca);
                    self.host.submit_command(&cmd9)?;
                    request.state = SdioInitState::PollCmd9;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd9 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(response) => {
                    request.capacity_blocks = match response {
                        Response::R2(raw) => CsdResponse::from_raw(raw).capacity_blocks(),
                        _ => None,
                    };
                    info!("sdio: CSD capacity_blocks={:?}", request.capacity_blocks);
                    let cmd7 = crate::cmd::cmd7(self.rca);
                    self.host.submit_command(&cmd7)?;
                    request.state = SdioInitState::PollCmd7;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollCmd7 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
                    self.high_capacity = ocr.ccs();
                    match request.kind.ok_or(Error::InvalidArgument)? {
                        CardKind::Sd => {
                            info!("sdio: switch SD bus width to 4-bit");
                            let cmd55 = crate::cmd::cmd55(self.rca);
                            self.host.submit_command(&cmd55)?;
                            request.state = SdioInitState::PollSdBusWidthCmd55;
                        }
                        CardKind::Mmc => {
                            request.state = SdioInitState::FinishCardSetup;
                        }
                    }
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollSdBusWidthCmd55 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => {
                    let acmd6 = Command::new(6, sd_acmd6_arg(BusWidth::Bit4)?, ResponseType::R1);
                    self.host.submit_command(&acmd6)?;
                    request.state = SdioInitState::PollSdBusWidthAcmd6;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollSdBusWidthAcmd6 => match self.host.poll_command_response()? {
                CommandResponsePoll::Pending => Ok(OperationPoll::Pending),
                CommandResponsePoll::Complete(_) => self.submit_init_bus_op(
                    request,
                    SdioBusOp::SetBusWidth(BusWidth::Bit4),
                    SdioInitState::PollSdHostBusWidth,
                ),
            },
            SdioInitState::PollSdHostBusWidth => {
                self.poll_init_bus_op_then(request, |driver, request| {
                    driver.bus_width = BusWidth::Bit4;
                    request.state = SdioInitState::FinishCardSetup;
                    Ok(OperationPoll::Pending)
                })
            }
            SdioInitState::FinishCardSetup => {
                let kind = request.kind.ok_or(Error::InvalidArgument)?;
                match kind {
                    CardKind::Sd => self.submit_init_bus_op(
                        request,
                        SdioBusOp::SetClock(ClockSpeed::Default),
                        SdioInitState::PollSdDefaultClock,
                    ),
                    CardKind::Mmc => {
                        debug!("sdio: read MMC EXT_CSD");
                        // SAFETY: the slot's debug_assert traps re-lending; the
                        // returned reference's lifetime is bound to the host's
                        // DataRequest via SwitchFunctionRequest/ExtCsdRequest,
                        // and we release on the Complete arm below.
                        let ext_csd = unsafe { request.ext_csd_buf.lend() };
                        request.ext_csd_request = Some(self.submit_read_ext_csd(ext_csd)?);
                        request.state = SdioInitState::PollMmcExtCsd;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollSdDefaultClock => {
                self.poll_init_bus_op_then(request, |driver, request| {
                    if driver.sd_speed_selection_enabled {
                        request.state = SdioInitState::PrepareSdSpeed;
                    } else {
                        debug!("sdio: SD speed selection disabled; staying at default speed");
                        request.state = SdioInitState::Complete;
                    }
                    Ok(OperationPoll::Pending)
                })
            }
            SdioInitState::PollMmcExtCsd => {
                let ext_request = request
                    .ext_csd_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_ext_csd_request(ext_request)? {
                    OperationPoll::Pending => Ok(OperationPoll::Pending),
                    OperationPoll::Complete(()) => {
                        request.ext_csd_request = None;
                        request.ext_csd_buf.release();
                        // SAFETY: we just released the slot above; the host
                        // has finished writing the buffer (DataCommandPoll::
                        // Complete is the host's promise) and nothing else
                        // holds a reference.
                        let csd = crate::ext_csd::ExtCsd::from_bytes(unsafe {
                            *request.ext_csd_buf.peek()
                        });
                        if let Some(sectors) = csd.sector_count() {
                            request.capacity_blocks = Some(sectors as u64);
                            info!("sdio: EXT_CSD sector_count={}", sectors);
                        }
                        request.parsed_ext_csd = Some(csd);
                        submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit8)
                    }
                }
            }
            SdioInitState::PollMmcBusWidth => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.mmc_switch_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SetBusWidth(request.current_bus_width))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollMmcHostBusWidth;
                                Ok(OperationPoll::Pending)
                            }
                            Err(err) => handle_mmc_host_bus_width_error(self, request, err),
                        }
                    }
                    Err(err) if matches!(request.current_bus_width, BusWidth::Bit8) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: 8-bit refused ({:?}), trying 4-bit", err);
                        submit_mmc_bus_width_or_continue(self, request, BusWidth::Bit4)
                    }
                    Err(err) if matches!(request.current_bus_width, BusWidth::Bit4) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: 4-bit refused ({:?}), staying at 1-bit", err);
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => Err(err),
                }
            }
            SdioInitState::PollMmcHostBusWidth => {
                let mut bus_request = request.bus_request.take().ok_or(Error::InvalidArgument)?;
                match self.host.poll_bus_op(&mut bus_request) {
                    Ok(OperationPoll::Pending) => {
                        request.bus_request = Some(bus_request);
                        Ok(OperationPoll::Pending)
                    }
                    Ok(OperationPoll::Complete(())) => {
                        self.bus_width = request.current_bus_width;
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => handle_mmc_host_bus_width_error(self, request, err),
                }
            }
            SdioInitState::PrepareMmcSpeed => {
                let Some(csd) = request.parsed_ext_csd.as_ref() else {
                    return Err(Error::InvalidArgument);
                };
                let dt = csd.device_type();
                if !request.mmc_hs200_attempted
                    && !matches!(self.bus_width, BusWidth::Bit1)
                    && dt.supports_hs200()
                {
                    request.mmc_hs200_attempted = true;
                    match self
                        .host
                        .submit_bus_op(SdioBusOp::SwitchVoltage(SignalVoltage::V180))
                    {
                        Ok(bus_request) => {
                            request.bus_request = Some(bus_request);
                            request.state = SdioInitState::PollMmcHs200VoltageSwitch;
                            return Ok(OperationPoll::Pending);
                        }
                        // The host has no way to actually drive the IO rail
                        // at 1.8 V (controllers like the rk3568 SDHCI MVP
                        // refuse here on purpose); HS200 requires 1.8 V, so
                        // skip the attempt entirely instead of leaving the
                        // controller's 1.8 V Signaling Enable bit set while
                        // running the bus at 3.3 V.
                        Err(Error::UnsupportedCommand) => {}
                        Err(err) => debug!("sdio: switch_voltage(V180) failed ({:?})", err),
                    }
                    self.rollback_to_hs_compat();
                }
                if dt.supports_hs_52() {
                    let switch_request =
                        self.submit_mmc_switch(0b11, crate::cmd::ext_csd::HS_TIMING as u8, 1)?;
                    request.mmc_switch_request = Some(switch_request);
                    request.state = SdioInitState::PollMmcHs52Switch;
                } else {
                    request.state = SdioInitState::Complete;
                }
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollMmcHs200VoltageSwitch => {
                let Some(csd) = request.parsed_ext_csd.as_ref() else {
                    return Err(Error::InvalidArgument);
                };
                let supports_hs52 = csd.device_type().supports_hs_52();
                match self.poll_init_bus_op(request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        let switch_request = self.submit_mmc_switch(
                            0b11,
                            crate::cmd::ext_csd::HS_TIMING as u8,
                            0x02,
                        )?;
                        request.mmc_switch_request = Some(switch_request);
                        request.state = SdioInitState::PollMmcHs200Switch;
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => {
                        debug!("sdio: switch_voltage(V180) failed ({:?})", err);
                        self.rollback_to_hs_compat();
                        if supports_hs52 {
                            let switch_request = self.submit_mmc_switch(
                                0b11,
                                crate::cmd::ext_csd::HS_TIMING as u8,
                                1,
                            )?;
                            request.mmc_switch_request = Some(switch_request);
                            request.state = SdioInitState::PollMmcHs52Switch;
                        } else {
                            request.state = SdioInitState::Complete;
                        }
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcHs200Switch => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.mmc_switch_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SetClock(ClockSpeed::Hs200))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollMmcHs200Clock;
                            }
                            Err(_) => {
                                self.rollback_to_hs_compat();
                                request.state = SdioInitState::PrepareMmcSpeed;
                            }
                        }
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: MMC HS200 switch refused ({:?})", err);
                        self.rollback_to_hs_compat();
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcHs200Clock => match self.poll_init_bus_op(request) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    let block_size = self.mmc_tuning_block_size()?;
                    match self.host.submit_bus_op(SdioBusOp::ExecuteTuning {
                        cmd_index: 21,
                        block_size,
                    }) {
                        Ok(bus_request) => {
                            request.bus_request = Some(bus_request);
                            request.state = SdioInitState::PollMmcHs200Tuning;
                        }
                        Err(_) => {
                            self.rollback_to_hs_compat();
                            request.state = SdioInitState::PrepareMmcSpeed;
                        }
                    }
                    Ok(OperationPoll::Pending)
                }
                Err(_) => {
                    self.rollback_to_hs_compat();
                    request.state = SdioInitState::PrepareMmcSpeed;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollMmcHs200Tuning => match self.poll_init_bus_op(request) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    let status_request = self.submit_status()?;
                    request.status_request = Some(status_request);
                    request.state = SdioInitState::PollMmcHs200Status;
                    Ok(OperationPoll::Pending)
                }
                Err(_) => {
                    self.rollback_to_hs_compat();
                    request.state = SdioInitState::PrepareMmcSpeed;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PollMmcHs200Status => {
                let status_request = request
                    .status_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_status_request(status_request)? {
                    OperationPoll::Pending => Ok(OperationPoll::Pending),
                    OperationPoll::Complete(CardState::Transfer) => {
                        request.status_request = None;
                        info!("sdio: HS200 entry succeeded");
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                    OperationPoll::Complete(_) => {
                        request.status_request = None;
                        self.rollback_to_hs_compat();
                        request.state = SdioInitState::PrepareMmcSpeed;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcHs52Switch => {
                let switch_request = request
                    .mmc_switch_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_mmc_switch_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.mmc_switch_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SetClock(ClockSpeed::HighSpeed))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollMmcHighSpeedClock;
                            }
                            Err(_e) => {
                                debug!("sdio: host refused HighSpeed clock ({:?})", _e);
                                request.state = SdioInitState::Complete;
                            }
                        }
                        Ok(OperationPoll::Pending)
                    }
                    Err(_e) => {
                        request.mmc_switch_request = None;
                        debug!("sdio: MMC HS_TIMING switch refused ({:?})", _e);
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollMmcHighSpeedClock => match self.poll_init_bus_op(request) {
                Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                Ok(OperationPoll::Complete(())) => {
                    info!(
                        "sdio: MMC speed selected HighSpeed bus_width={:?}",
                        self.bus_width
                    );
                    request.state = SdioInitState::Complete;
                    Ok(OperationPoll::Pending)
                }
                Err(_e) => {
                    debug!("sdio: host refused HighSpeed clock ({:?})", _e);
                    request.state = SdioInitState::Complete;
                    Ok(OperationPoll::Pending)
                }
            },
            SdioInitState::PrepareSdSpeed => {
                // SAFETY: see ext_csd lend above; release happens on the
                // PollSdSwitchFunctionCheck Complete arm below.
                let buf = unsafe { request.switch_status_buf.lend() };
                let switch_request =
                    self.submit_switch_function(&crate::cmd::cmd6_sd_access_mode(false, 0), buf)?;
                request.switch_function_request = Some(switch_request);
                request.state = SdioInitState::PollSdSwitchFunctionCheck;
                Ok(OperationPoll::Pending)
            }
            SdioInitState::PollSdSwitchFunctionCheck => {
                let switch_request = request
                    .switch_function_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_switch_function_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.switch_function_request = None;
                        request.switch_status_buf.release();
                        // SAFETY: just released above; host promised the data
                        // phase is done via DataCommandPoll::Complete.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        debug!(
                            "sdio: SD access mode support hs={} sdr50={} sdr104={} ddr50={} \
                             s18a={}",
                            status.access_mode_supported(SdAccessMode::HighSpeed.function()),
                            status.access_mode_supported(SdAccessMode::Sdr50.function()),
                            status.access_mode_supported(SdAccessMode::Sdr104.function()),
                            status.access_mode_supported(SdAccessMode::Ddr50.function()),
                            request.ocr.ok_or(Error::InvalidArgument)?.s18a()
                        );
                        request.sd_access_index = 0;
                        submit_next_sd_access_mode(self, request, status)
                    }
                    Err(err) => {
                        request.switch_function_request = None;
                        request.switch_status_buf.release();
                        warn!("sdio: SD speed selection skipped ({:?})", err);
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                }
            }
            SdioInitState::PollSdVoltageSwitch => {
                let cmd = request
                    .command_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_command_request(cmd) {
                    Ok(CommandResponsePoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(CommandResponsePoll::Complete(_)) => {
                        request.command_request = None;
                        match self
                            .host
                            .submit_bus_op(SdioBusOp::SwitchVoltage(SignalVoltage::V180))
                        {
                            Ok(bus_request) => {
                                request.bus_request = Some(bus_request);
                                request.state = SdioInitState::PollSdSignalVoltage;
                                Ok(OperationPoll::Pending)
                            }
                            Err(err) => {
                                warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                // SAFETY: no switch_function_request is in
                                // flight on this branch (CMD11 path uses the
                                // command channel), so the slot is not lent.
                                let status = SwitchStatus::from_raw(unsafe {
                                    *request.switch_status_buf.peek()
                                });
                                submit_next_sd_access_mode(self, request, status)
                            }
                        }
                    }
                    Err(err) => {
                        request.command_request = None;
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: same as above — no in-flight data request.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdSignalVoltage => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_init_bus_op(request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        submit_sd_access_mode_switch(self, request, mode)
                    }
                    Err(err) => {
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: no switch_function_request is in flight on
                        // this branch; the switch-status scratch slot was
                        // released after the earlier function-check request.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdSetAccessMode => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                let switch_request = request
                    .switch_function_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_switch_function_request(switch_request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        request.switch_function_request = None;
                        request.switch_status_buf.release();
                        // SAFETY: just released above.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        if status.selected_function(1) != mode.function() {
                            warn!("sdio: SD {} failed (function mismatch)", mode.name());
                            submit_next_sd_access_mode(self, request, status)
                        } else {
                            match self.host.submit_bus_op(SdioBusOp::SetClock(mode.clock())) {
                                Ok(bus_request) => {
                                    request.bus_request = Some(bus_request);
                                    request.state = SdioInitState::PollSdClock;
                                    Ok(OperationPoll::Pending)
                                }
                                Err(err) => {
                                    warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                    submit_next_sd_access_mode(self, request, status)
                                }
                            }
                        }
                    }
                    Err(err) => {
                        request.switch_function_request = None;
                        request.switch_status_buf.release();
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: just released above.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdClock => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_init_bus_op(request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        if matches!(mode, SdAccessMode::Sdr50 | SdAccessMode::Sdr104) {
                            let block_size = self.sd_tuning_block_size()?;
                            match self.host.submit_bus_op(SdioBusOp::ExecuteTuning {
                                cmd_index: 19,
                                block_size,
                            }) {
                                Ok(bus_request) => {
                                    request.bus_request = Some(bus_request);
                                    request.state = SdioInitState::PollSdTuning;
                                    Ok(OperationPoll::Pending)
                                }
                                Err(err) => {
                                    warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                                    // SAFETY: PollSdClock is reached after the
                                    // switch request released the status slot.
                                    let status = SwitchStatus::from_raw(unsafe {
                                        *request.switch_status_buf.peek()
                                    });
                                    submit_next_sd_access_mode(self, request, status)
                                }
                            }
                        } else {
                            let status_request = self.submit_status()?;
                            request.status_request = Some(status_request);
                            request.state = SdioInitState::PollSdStatus;
                            Ok(OperationPoll::Pending)
                        }
                    }
                    Err(err) => {
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: PollSdClock is reached after the switch
                        // request released the status slot.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdTuning => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                match self.poll_init_bus_op(request) {
                    Ok(OperationPoll::Pending) => Ok(OperationPoll::Pending),
                    Ok(OperationPoll::Complete(())) => {
                        let status_request = self.submit_status()?;
                        request.status_request = Some(status_request);
                        request.state = SdioInitState::PollSdStatus;
                        Ok(OperationPoll::Pending)
                    }
                    Err(err) => {
                        warn!("sdio: SD {} failed ({:?})", mode.name(), err);
                        // SAFETY: PollSdTuning is reached after the switch
                        // request released the status slot.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::PollSdStatus => {
                let mode = request.current_access_mode.ok_or(Error::InvalidArgument)?;
                let status_request = request
                    .status_request
                    .as_mut()
                    .ok_or(Error::InvalidArgument)?;
                match self.poll_status_request(status_request)? {
                    OperationPoll::Pending => Ok(OperationPoll::Pending),
                    OperationPoll::Complete(CardState::Transfer) => {
                        request.status_request = None;
                        info!("sdio: SD speed selected {:?}", mode.clock());
                        request.state = SdioInitState::Complete;
                        Ok(OperationPoll::Pending)
                    }
                    OperationPoll::Complete(_) => {
                        request.status_request = None;
                        warn!("sdio: SD {} failed (bad status)", mode.name());
                        // SAFETY: PollSdStatus is reached after the switch
                        // request released the slot in PollSdSetAccessMode;
                        // no data request is in flight.
                        let status =
                            SwitchStatus::from_raw(unsafe { *request.switch_status_buf.peek() });
                        submit_next_sd_access_mode(self, request, status)
                    }
                }
            }
            SdioInitState::Complete => {
                let kind = request.kind.ok_or(Error::InvalidArgument)?;
                let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
                let ext_csd_timing = request.parsed_ext_csd.as_ref().map(|csd| csd.timing());
                let ext_csd_bus_width = request.parsed_ext_csd.as_ref().map(|csd| csd.bus_width());
                info!(
                    "sdio: init done kind={:?} sd_v2={} high_capacity={} rca={:#x} ocr={:#x} \
                     host_bus_width={:?} ext_csd_bus_width={:?} ext_csd_timing={:?}",
                    kind,
                    request.sd_v2,
                    self.high_capacity,
                    self.rca,
                    ocr.raw,
                    self.bus_width,
                    ext_csd_bus_width,
                    ext_csd_timing
                );
                Ok(OperationPoll::Complete(CardInfo {
                    kind,
                    sd_v2: request.sd_v2,
                    high_capacity: self.high_capacity,
                    ocr: ocr.raw,
                    rca: self.rca,
                    capacity_blocks: request.capacity_blocks,
                    cid: request.cid,
                    ext_csd: request.parsed_ext_csd.take(),
                }))
            }
        }
    }

    /// Best-effort host + driver reset after a failed or abandoned init.
    ///
    /// Init can leave the controller in any number of partially-programmed
    /// states: 4-bit/8-bit bus already negotiated, clock raised to HS@52,
    /// HOST_CONTROL2 UHS bits set from a HS200 attempt, 1.8 V signaling
    /// armed. None of those are safe defaults for a subsequent retry that
    /// expects to start by replaying CMD0 in identification mode.
    ///
    /// This helper:
    ///
    /// - Asks the host to drop back to identification clock, 1-bit bus, and
    ///   3.3 V signaling. Errors from each call are swallowed — we're
    ///   already on the error path and want maximum cleanup, not a second
    ///   failure mid-recovery.
    /// - Clears the driver's cached card state (RCA, kind, bus width,
    ///   high-capacity flag) so subsequent calls don't act on stale data
    ///   from the aborted card.
    ///
    /// Idempotent: calling it twice or on a fresh driver is a no-op
    /// modulo the (already-defaulted) field stores.
    fn abort_init(&mut self) {
        let _ = self.host.switch_voltage(SignalVoltage::V330);
        let _ = self.host.set_clock(ClockSpeed::Identification);
        let _ = self.host.set_bus_width(BusWidth::Bit1);
        self.rca = 0;
        self.high_capacity = false;
        self.bus_width = BusWidth::Bit1;
        self.kind = CardKind::Sd;
    }

    /// Best-effort rollback after a failed HS200 attempt. Drops the
    /// controller clock back to default speed; the outer `init` will
    /// then re-program HS_TIMING=1 + HighSpeed in its fallback branch.
    /// Errors are deliberately swallowed — we're already on the error
    /// path and want to give the rest of `init` the best shot at
    /// recovering.
    fn rollback_to_hs_compat(&mut self) {
        // Drop any 1.8 V signaling the HS200 attempt may have armed on the
        // controller. Without this, the IO sampling stays at the 1.8 V
        // reference while we drive the bus back at 3.3 V, so the very next
        // data transfer (e.g. the FS layer's CMD17 at LBA 0) times out.
        let _ = self.host.switch_voltage(SignalVoltage::V330);
        let _ = self.host.set_clock(ClockSpeed::Default);
    }
}

fn submit_mmc_bus_width_or_continue<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    width: BusWidth,
) -> Result<OperationPoll<CardInfo>, Error> {
    let value: u8 = match width {
        BusWidth::Bit1 => 0,
        BusWidth::Bit4 => 1,
        BusWidth::Bit8 => 2,
        _ => return Err(Error::UnsupportedCommand),
    };
    request.current_bus_width = width;
    request.mmc_switch_request =
        Some(driver.submit_mmc_switch(0b11, crate::cmd::ext_csd::BUS_WIDTH as u8, value)?);
    request.state = SdioInitState::PollMmcBusWidth;
    Ok(OperationPoll::Pending)
}

fn handle_mmc_host_bus_width_error<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    err: Error,
) -> Result<OperationPoll<CardInfo>, Error> {
    request.bus_request = None;
    if matches!(request.current_bus_width, BusWidth::Bit8) {
        debug!("sdio: 8-bit refused ({:?}), trying 4-bit", err);
        submit_mmc_bus_width_or_continue(driver, request, BusWidth::Bit4)
    } else if matches!(request.current_bus_width, BusWidth::Bit4) {
        debug!("sdio: 4-bit refused ({:?}), staying at 1-bit", err);
        request.state = SdioInitState::PrepareMmcSpeed;
        Ok(OperationPoll::Pending)
    } else {
        Err(err)
    }
}

fn submit_next_sd_access_mode<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    status: SwitchStatus,
) -> Result<OperationPoll<CardInfo>, Error> {
    let ocr = request.ocr.ok_or(Error::InvalidArgument)?;
    let candidates = if driver.sd_uhs_selection_enabled && ocr.s18a() {
        &[
            SdAccessMode::Sdr104,
            SdAccessMode::Sdr50,
            SdAccessMode::Ddr50,
            SdAccessMode::HighSpeed,
        ][..]
    } else {
        &[SdAccessMode::HighSpeed][..]
    };

    while request.sd_access_index < candidates.len() {
        let mode = candidates[request.sd_access_index];
        request.sd_access_index += 1;
        if !status.access_mode_supported(mode.function()) {
            continue;
        }
        if matches!(mode, SdAccessMode::HighSpeed) {
            debug!("sdio: trying SD HighSpeed");
        } else {
            debug!("sdio: trying SD {}", mode.name());
        }
        return submit_sd_access_mode(driver, request, mode);
    }

    debug!("sdio: SD card stayed at default speed");
    request.state = SdioInitState::Complete;
    Ok(OperationPoll::Pending)
}

fn submit_sd_access_mode<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    mode: SdAccessMode,
) -> Result<OperationPoll<CardInfo>, Error> {
    request.current_access_mode = Some(mode);
    if !matches!(mode, SdAccessMode::HighSpeed) && request.ocr.ok_or(Error::InvalidArgument)?.s18a()
    {
        let cmd = crate::cmd::CMD11;
        request.command_request = Some(driver.submit_command_request(&cmd)?);
        request.state = SdioInitState::PollSdVoltageSwitch;
        return Ok(OperationPoll::Pending);
    }

    submit_sd_access_mode_switch(driver, request, mode)
}

fn submit_sd_access_mode_switch<'a, H: SdioHost + 'a>(
    driver: &mut SdioSdmmc<H>,
    request: &mut SdioInitRequest<'a, H>,
    mode: SdAccessMode,
) -> Result<OperationPoll<CardInfo>, Error> {
    // SAFETY: the prior switch_function_request was either consumed and
    // released in PollSdSwitchFunctionCheck Complete, or never lent (CMD11
    // voltage-switch failure path); release defensively so a re-entered
    // path doesn't keep the slot flagged.
    request.switch_status_buf.release();
    let buf = unsafe { request.switch_status_buf.lend() };
    request.switch_function_request = Some(
        driver
            .submit_switch_function(&crate::cmd::cmd6_sd_access_mode(true, mode.function()), buf)?,
    );
    request.state = SdioInitState::PollSdSetAccessMode;
    Ok(OperationPoll::Pending)
}

fn block_count_from_len(len: usize) -> Result<u32, Error> {
    if len == 0 || !len.is_multiple_of(512) {
        return Err(Error::Misaligned);
    }
    u32::try_from(len / 512).map_err(|_| Error::InvalidArgument)
}

fn sd_acmd6_arg(width: BusWidth) -> Result<u32, Error> {
    match width {
        BusWidth::Bit1 => Ok(0),
        BusWidth::Bit4 => Ok(2),
        BusWidth::Bit8 => Err(Error::UnsupportedCommand),
        _ => Err(Error::UnsupportedCommand),
    }
}

/// Card information obtained during SDIO initialization
#[derive(Debug, Clone)]
pub struct CardInfo {
    /// Which physical-layer protocol the card speaks. SD vs eMMC matters
    /// for follow-up steps the protocol layer can't generalize over —
    /// e.g. EXT_CSD reads, 8-bit bus switching, HS200 tuning.
    pub kind: CardKind,
    /// True when the card responded to CMD8 with a valid R7 echo
    /// (SD physical layer 2.0+). Always `false` for eMMC.
    pub sd_v2: bool,
    pub high_capacity: bool,
    pub ocr: u32,
    pub rca: u16,
    /// User-data capacity in 512-byte blocks, parsed from the CSD.
    /// `None` if the CSD reports a structure version we do not yet support.
    pub capacity_blocks: Option<u64>,
    /// Card identification register (manufacturer / OEM / serial / date).
    /// `None` if the host returned an unexpected response type to CMD2.
    pub cid: Option<CidResponse>,
    /// Decoded EXT_CSD register, present only for [`CardKind::Mmc`]
    /// after a successful init. Lets callers introspect HS200/HS400
    /// support, partition geometry, etc., without re-reading the card.
    pub ext_csd: Option<crate::ext_csd::ExtCsd>,
}

/// Which physical-layer family the card belongs to.
///
/// The SD vs MMC split is decided during `init()`:
///
/// - CMD8 echoes a valid R7 → SD v2 (SDHC/SDXC)
/// - CMD8 has no response, but ACMD41 succeeds → SD v1 (legacy SDSC)
/// - CMD8 has no response and ACMD41 also fails, but CMD1 reports
///   power-up → eMMC / MMC
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum CardKind {
    /// SD memory card (SDSC / SDHC / SDXC).
    Sd,
    /// Embedded MMC or removable MMC card.
    Mmc,
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use super::*;
    use crate::response::{IfCondResponse, OcrResponse, R1Response, RcaResponse};

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
