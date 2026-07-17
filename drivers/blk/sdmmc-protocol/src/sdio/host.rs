//! SDIO host-controller capability boundary.

use core::{num::NonZeroU16, task::Waker};

pub use sdio_host2::{BusWidth, ClockSpeed, SignalVoltage};

use crate::{
    block::{BlockRequestId, CommandResponsePoll, DataCommandPoll, OperationPoll},
    cmd::Command,
    error::Error,
};

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

/// Result of retrying one IRQ snapshot from the bounded task-context
/// continuation selected by [`HostEvent::ack_deferred`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeferredIrqAck {
    /// The retry acquired the register block but found no pending device
    /// source. No initialization or request state may advance from the old
    /// deferred notification.
    Unhandled,
    /// The IRQ endpoint acquired the register block and acknowledged any
    /// pending source into queue-local cached state. This variant requires a
    /// non-empty host event produced by the destructive snapshot.
    Acknowledged,
    /// Another short register update still owns the block; the fixed work item
    /// must be requeued without inspecting request completion state.
    Contended,
}

impl DeferredIrqAck {
    pub fn from_event(event: &impl HostEvent) -> Self {
        if event.ack_deferred() {
            Self::Contended
        } else if event.kind() == HostEventKind::None {
            Self::Unhandled
        } else {
            Self::Acknowledged
        }
    }
}

/// Stable event summary extracted by a host controller IRQ handler.
pub trait HostEvent {
    fn kind(&self) -> HostEventKind;

    /// Reports that destructive IRQ acknowledgement was deliberately deferred
    /// because task context currently owns the controller register block.
    fn ack_deferred(&self) -> bool {
        false
    }

    fn source(&self) -> HostEventSource {
        HostEventSource::Controller
    }

    fn queue_id(&self) -> Option<BlockRequestId> {
        None
    }

    /// Whether this acknowledged event can advance the serialized block
    /// request state machine.
    ///
    /// Controller sideband events may be handled without scheduling queue
    /// service. The default preserves the legacy single-queue behaviour for
    /// hosts that do not classify sideband status separately.
    fn requests_block_queue_service(&self) -> bool {
        self.kind() != HostEventKind::None
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

/// IRQ-endpoint extension of [`SdioHost`].
///
/// Hardware runtime queues require this endpoint. The top half clears the
/// device-side source and records a stable snapshot; bounded task context then
/// advances the request from that snapshot without re-reading destructive
/// interrupt status.
pub trait SdioIrqHost: SdioHost {
    type IrqHandle: SdioIrqHandle<Event = Self::Event>;

    fn irq_handle(&mut self) -> Self::IrqHandle;
}

/// Queue identifier used by SD/MMC block adapters.
pub const SDMMC_BLOCK_QUEUE_ID: usize = 0;

/// Convert a host IRQ event into the fixed SD/MMC block queue hint.
///
/// SD/MMC adapters expose one RDIF block queue per controller in this
/// workspace. Request-relevant events map to queue 0, while acknowledged
/// controller sideband events remain handled without queue activation. RDIF
/// completion still happens only when serialized task context consumes the
/// acknowledged event batch.
pub fn block_queue_ready_from_host_event(event: &impl HostEvent) -> Option<usize> {
    if event.requests_block_queue_service() {
        Some(SDMMC_BLOCK_QUEUE_ID)
    } else {
        None
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

    /// Advance a command using the caller's absolute monotonic time.
    ///
    /// The default is suitable only for hosts whose command programming never
    /// contains an eventless timed transition.
    fn poll_command_response_at(&mut self, now_ns: u64) -> Result<CommandResponsePoll, Error> {
        let _ = now_ns;
        self.poll_command_response()
    }

    /// Absolute activation required before command programming may continue.
    ///
    /// This deadline permits another state transition; it is never evidence
    /// that the command completed.
    fn command_wake_at(&self) -> Option<u64> {
        None
    }

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

    /// Advance a data command using the caller's absolute monotonic time.
    fn poll_data_request_at<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
        now_ns: u64,
    ) -> Result<DataCommandPoll, Error> {
        let _ = now_ns;
        self.poll_data_request(request)
    }

    /// Absolute activation required before data-command programming may
    /// continue. Completion still requires an acknowledged controller IRQ.
    fn data_request_wake_at<'a>(&self, _request: &Self::DataRequest<'a>) -> Option<u64> {
        None
    }

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

    /// Advance a bus operation with the caller's absolute monotonic time.
    ///
    /// Hosts whose bus transitions are entirely register-driven may keep the
    /// default implementation. Hosts with eventless platform sequencing use
    /// `now_ns` to advance an explicit state machine; they must not obtain a
    /// hidden clock or busy-wait inside this callback.
    fn poll_bus_op_at(
        &mut self,
        request: &mut Self::BusRequest,
        now_ns: u64,
    ) -> Result<OperationPoll<()>, Error> {
        let _ = now_ns;
        self.poll_bus_op(request)
    }

    /// Absolute activation requested by the current bus-operation state.
    ///
    /// `None` asks the protocol's bounded eventless-transition scheduler to
    /// choose its normal next check. A returned deadline is never interpreted
    /// as successful hardware completion; it only permits another state-machine
    /// transition.
    fn bus_op_wake_at(&self, _request: &Self::BusRequest) -> Option<u64> {
        None
    }

    /// Route command/data completion and error status to the host IRQ line.
    fn enable_completion_irq(&mut self) -> Result<(), Error>;

    /// Mask host IRQ delivery before recovery or ownership transfer.
    fn disable_completion_irq(&mut self) -> Result<(), Error>;

    fn completion_irq_enabled(&self) -> bool;

    /// Register the task that should be woken when command or data progress is
    /// possible. Runtime block queues use their owned IRQ endpoint instead.
    fn register_waker(&mut self, _waker: &Waker) {}

    /// Legacy host-local millisecond clock.
    ///
    /// Card initialization now consumes only caller-provided
    /// [`super::InitInput::now_ns`], so it never calls this method. The hook is
    /// retained for source compatibility with host operations outside the
    /// initialization FSM and may be removed after those users migrate to an
    /// explicit absolute-time input.
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
