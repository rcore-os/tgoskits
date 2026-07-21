//! SDIO host-controller capability boundary.

use core::{fmt, num::NonZeroU16};

use rdif_irq::{IrqEndpoint, IrqSourceControl};
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

/// Plain acknowledged facts exported by an SD/MMC host IRQ endpoint.
///
/// This value contains no callback, allocation, or resource ownership. A
/// higher-level portable driver may therefore translate it into its own event
/// type directly in the hard-IRQ capture path without calling OS glue.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct HostIrqSnapshot {
    /// Complete host-specific status retained for bounded owner-side service
    /// and diagnostics.
    pub stable_status: u32,
    /// Host-specific DMA-engine status acknowledged by the same IRQ capture.
    /// Hosts without a distinct DMA status register leave this zero.
    pub dma_status: u32,
    /// A command, data, or error fact can advance the serialized host queue.
    pub queue_service: bool,
    /// An SDIO function asserted the card interrupt source.
    pub card_function_interrupt: bool,
}

impl HostIrqSnapshot {
    /// Merges coalesced snapshots from one serialized controller source.
    pub const fn merge(self, other: Self) -> Self {
        Self {
            stable_status: self.stable_status | other.stable_status,
            dma_status: self.dma_status | other.dma_status,
            queue_service: self.queue_service || other.queue_service,
            card_function_interrupt: self.card_function_interrupt || other.card_function_interrupt,
        }
    }

    /// Whether this snapshot contains no acknowledged hardware fact.
    pub const fn is_empty(self) -> bool {
        self.stable_status == 0
            && self.dma_status == 0
            && !self.queue_service
            && !self.card_function_interrupt
    }
}

/// Compatibility name for callers that only classify an event.
pub type HostEventSummary = HostIrqSnapshot;

/// Stable event summary extracted by a host controller IRQ handler.
pub trait HostEvent {
    fn kind(&self) -> HostEventKind;

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

    /// Returns the callback-free facts needed by a nested SDIO function
    /// driver.
    ///
    /// Hosts that expose raw acknowledged status or a card-function interrupt
    /// override this method. The default keeps existing host event types
    /// source-compatible while preserving their queue-service semantics.
    fn stable_summary(&self) -> HostEventSummary {
        HostEventSummary {
            stable_status: 0,
            dma_status: 0,
            queue_service: self.requests_block_queue_service(),
            card_function_interrupt: false,
        }
    }
}

impl HostEvent for () {
    fn kind(&self) -> HostEventKind {
        HostEventKind::None
    }
}

/// Split ownership of one SD/MMC controller interrupt source.
///
/// `endpoint` is moved into the OS hard-IRQ action. `control` remains with the
/// CPU-pinned maintenance owner and may only rearm generation-checked source
/// bits after the captured event has been consumed. Neither capability grants
/// access to the task-side command/data endpoint.
pub struct SdioIrqSource<E, C> {
    endpoint: E,
    control: C,
}

/// Failure while the maintenance owner rearms a device-masked source.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SdioIrqControlError {
    /// The token belongs to an older controller activation.
    StaleGeneration { expected: u64, actual: u64 },
    /// The token names source bits that this capture did not leave masked.
    SourceNotMasked { bitmap: u64 },
    /// Recovery or shutdown removed the source owner.
    Offline,
    /// Controller-specific register programming failed.
    Hardware(Error),
}

impl fmt::Display for SdioIrqControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleGeneration { expected, actual } => {
                write!(
                    formatter,
                    "stale SD/MMC IRQ generation: expected {expected}, got {actual}"
                )
            }
            Self::SourceNotMasked { bitmap } => {
                write!(formatter, "SD/MMC IRQ source {bitmap:#x} is not masked")
            }
            Self::Offline => formatter.write_str("SD/MMC IRQ source owner is offline"),
            Self::Hardware(error) => write!(formatter, "SD/MMC IRQ control failed: {error}"),
        }
    }
}

impl core::error::Error for SdioIrqControlError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            Self::Hardware(error) => Some(error),
            Self::StaleGeneration { .. } | Self::SourceNotMasked { .. } | Self::Offline => None,
        }
    }
}

impl<E, C> SdioIrqSource<E, C> {
    /// Constructs a source from independently owned capture and rearm parts.
    pub const fn new(endpoint: E, control: C) -> Self {
        Self { endpoint, control }
    }

    /// Transfers both capabilities to the OS integration layer.
    pub fn into_parts(self) -> (E, C) {
        (self.endpoint, self.control)
    }
}

/// IRQ-endpoint extension of [`SdioHost`].
///
/// Hardware runtime queues require this endpoint. The endpoint is the only
/// runtime owner allowed to read or W1C command, data, DMA, and error interrupt
/// status. It must acknowledge a bounded snapshot without allocation or lock
/// acquisition, then return ordinary-memory facts. A register-ownership
/// conflict is an activation bug: the endpoint must contain its exact source
/// or report an uncontained fault, never ask task context to retry capture.
///
/// The maintenance thread owns the host and control endpoint on the same CPU.
/// It programs registers with the device source or local IRQ excluded, consumes
/// only the endpoint's cached facts, and rearms a masked source with the exact
/// generation token. Implementations must not construct another source while a
/// previous source generation remains registered.
pub trait SdioIrqHost: SdioHost {
    /// Hard-IRQ-owned destructive status capture endpoint.
    type IrqEndpoint: IrqEndpoint<Event = Self::Event, Fault = Error>;
    /// Maintenance-owner capability for generation-checked source rearming.
    type IrqControl: IrqSourceControl<Error = SdioIrqControlError>;

    /// Transfers the controller's unique IRQ source exactly once per active
    /// source generation.
    fn take_irq_source(&mut self) -> Option<SdioIrqSource<Self::IrqEndpoint, Self::IrqControl>>;

    /// Transfers a source whose endpoint publishes facts only through the
    /// caller-owned evidence ledger.
    ///
    /// Legacy hosts may return their normal source while they are being
    /// migrated. A v0.13 activation must use a host implementation that
    /// overrides this method and disables its legacy task-side IRQ mailbox.
    fn take_evidence_irq_source(
        &mut self,
    ) -> Option<SdioIrqSource<Self::IrqEndpoint, Self::IrqControl>> {
        self.take_irq_source()
    }
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

    /// Advance a submitted command from status cached by its IRQ endpoint and
    /// harvest the response when complete.
    ///
    /// Runtime implementations must not read or clear destructive interrupt
    /// status here. A missing completion IRQ is handled by the caller's
    /// watchdog and controller recovery, not by status polling.
    fn poll_command_response(&mut self) -> Result<CommandResponsePoll, Error>;

    /// Advances a command using exactly one snapshot claimed from the driver
    /// evidence ledger.
    ///
    /// Implementations must not consult a destructive register or a second
    /// IRQ mailbox while servicing this call.
    fn poll_command_response_with_snapshot(
        &mut self,
        _snapshot: HostIrqSnapshot,
    ) -> Result<CommandResponsePoll, Error> {
        Err(Error::UnsupportedCommand)
    }

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

    /// Advance a previously submitted data command from cached IRQ facts
    /// without blocking or re-reading destructive interrupt status.
    fn poll_data_request<'a>(
        &mut self,
        request: &mut Self::DataRequest<'a>,
    ) -> Result<DataCommandPoll, Error>;

    /// Advances a data request using exactly one snapshot claimed from the
    /// driver evidence ledger.
    fn poll_data_request_with_snapshot<'a>(
        &mut self,
        _request: &mut Self::DataRequest<'a>,
        _snapshot: HostIrqSnapshot,
    ) -> Result<DataCommandPoll, Error> {
        Err(Error::UnsupportedCommand)
    }

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
