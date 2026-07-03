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
