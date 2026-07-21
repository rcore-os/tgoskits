//! SD/MMC card initialization state machine.

use log::{debug, info, warn};

use super::{
    card::{
        CardInfo, CardKind, ExtCsdRequest, SdioCommandRequest, SdioSdmmc, SdioStatusRequest,
        SwitchFunctionRequest,
    },
    host::{BusWidth, ClockSpeed, HostIrqSnapshot, SdioBusOp, SdioHost, SignalVoltage},
    init_schedule::{InitInput, InitPoll, InitSchedule},
};
use crate::{
    block::{CommandResponsePoll, OperationPoll},
    cmd::Command,
    error::{Error, ErrorContext, Phase},
    response::{
        CardState, CidResponse, CsdResponse, OcrResponse, Response, ResponseType, SwitchStatus,
    },
};

mod activation;
mod bootstrap;
mod card_setup;
mod discovery;
mod mmc_speed;
mod sd_speed;
mod terminal;

use activation::next_init_activation;
pub(super) use sd_speed::sd_acmd6_arg;

pub(super) const INIT_RETRY_INTERVAL_NS: u64 = 10_000_000;
pub(super) const INIT_POWER_UP_TIMEOUT_NS: u64 = 1_000_000_000;
pub(super) const MMC_SWITCH_TIMEOUT_NS: u64 = 250_000_000;
const INIT_EVENTLESS_POLL_NS: u64 = 50_000;
const INIT_EVENTLESS_TIMEOUT_NS: u64 = 1_000_000_000;
const INIT_IRQ_TIMEOUT_NS: u64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TimerActivation {
    Advance,
    Timeout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingActivation {
    schedule: InitSchedule,
    timer: TimerActivation,
}

impl PendingActivation {
    const fn immediate() -> Self {
        Self {
            schedule: InitSchedule::immediate(),
            timer: TimerActivation::Advance,
        }
    }

    const fn wait_until(wake_at_ns: u64) -> Self {
        Self {
            schedule: InitSchedule::wait_until(wake_at_ns),
            timer: TimerActivation::Advance,
        }
    }

    const fn wait_for_irq(deadline_ns: u64) -> Self {
        Self {
            schedule: InitSchedule::wait_for_controller_irq(deadline_ns),
            timer: TimerActivation::Timeout,
        }
    }

    const fn wait_for_irq_or_until(wake_at_ns: u64) -> Self {
        Self {
            schedule: InitSchedule {
                run_again: false,
                irq: super::init_schedule::InitIrqWait::Controller,
                wake_at_ns: Some(wake_at_ns),
            },
            timer: TimerActivation::Advance,
        }
    }

    const fn timeout_at(deadline_ns: u64) -> Self {
        Self {
            schedule: InitSchedule::wait_until(deadline_ns),
            timer: TimerActivation::Timeout,
        }
    }

    fn activation(self, input: InitInput) -> Activation {
        if self.schedule.run_again
            || (input.has_controller_irq()
                && matches!(
                    self.schedule.irq,
                    super::init_schedule::InitIrqWait::Controller
                ))
        {
            return Activation::Advance;
        }
        match self.schedule.wake_at_ns {
            Some(wake_at_ns) if input.now_ns >= wake_at_ns => match self.timer {
                TimerActivation::Advance => Activation::Advance,
                TimerActivation::Timeout => Activation::Timeout,
            },
            _ => Activation::Waiting,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Activation {
    Waiting,
    Advance,
    Timeout,
}

pub struct MmcSwitchRequest {
    pub(super) rca: u16,
    pub(super) index: u8,
    pub(super) value: u8,
    pub(super) deadline_ns: u64,
    pub(super) retry_at_ns: Option<u64>,
    pub(super) state: MmcSwitchRequestState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MmcSwitchRequestState {
    PollSwitch,
    PollStatus,
    WaitStatusRetry,
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
pub(super) struct ScratchSlot<const N: usize> {
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
    pub(super) state: SdioInitState,
    pub(super) preference: CardInitPreference,
    pub(super) sd_v2: bool,
    pub(super) kind: Option<CardKind>,
    pub(super) ocr: Option<OcrResponse>,
    pub(super) cid: Option<CidResponse>,
    pub(super) capacity_blocks: Option<u64>,
    pub(super) parsed_ext_csd: Option<crate::ext_csd::ExtCsd>,
    pub(super) power_deadline_ns: Option<u64>,
    pub(super) retry_at_ns: Option<u64>,
    pub(super) mmc_ocr_arg: u32,
    pending_activation: Option<PendingActivation>,
    last_now_ns: Option<u64>,
    deadline_state: Option<SdioInitState>,
    hardware_deadline_ns: Option<u64>,
    terminal_error: Option<Error>,
    irq_snapshot: Option<HostIrqSnapshot>,
    pub(super) ext_csd_buf: ScratchSlot<512>,
    pub(super) switch_status_buf: ScratchSlot<64>,
    pub(super) ext_csd_request: Option<ExtCsdRequest<'a, H>>,
    pub(super) switch_function_request: Option<SwitchFunctionRequest<'a, H>>,
    pub(super) mmc_switch_request: Option<MmcSwitchRequest>,
    pub(super) status_request: Option<SdioStatusRequest>,
    pub(super) command_request: Option<SdioCommandRequest>,
    pub(super) bus_request: Option<H::BusRequest>,
    pub(super) bus_wake_at_ns: Option<u64>,
    pub(super) transaction_wake_at_ns: Option<u64>,
    pub(super) active_bus_op: Option<SdioBusOp>,
    pub(super) current_bus_width: BusWidth,
    pub(super) current_access_mode: Option<SdAccessMode>,
    pub(super) sd_access_index: usize,
    pub(super) mmc_hs200_attempted: bool,
    pub(super) _scratch: core::marker::PhantomData<&'a mut SdioInitScratch>,
}

impl<'a, H: SdioHost + 'a> SdioInitRequest<'a, H> {
    pub(super) fn new(preference: CardInitPreference, scratch: &'a mut SdioInitScratch) -> Self {
        Self {
            state: SdioInitState::ResetHost,
            preference,
            sd_v2: false,
            kind: None,
            ocr: None,
            cid: None,
            capacity_blocks: None,
            parsed_ext_csd: None,
            power_deadline_ns: None,
            retry_at_ns: None,
            mmc_ocr_arg: 0,
            pending_activation: None,
            last_now_ns: None,
            deadline_state: None,
            hardware_deadline_ns: None,
            terminal_error: None,
            irq_snapshot: None,
            ext_csd_buf: ScratchSlot::new(&mut scratch.ext_csd),
            switch_status_buf: ScratchSlot::new(&mut scratch.switch_status),
            ext_csd_request: None,
            switch_function_request: None,
            mmc_switch_request: None,
            status_request: None,
            command_request: None,
            bus_request: None,
            bus_wake_at_ns: None,
            transaction_wake_at_ns: None,
            active_bus_op: None,
            current_bus_width: BusWidth::Bit1,
            current_access_mode: None,
            sd_access_index: 0,
            mmc_hs200_attempted: false,
            _scratch: core::marker::PhantomData,
        }
    }

    fn take_irq_snapshot(&mut self) -> Option<HostIrqSnapshot> {
        self.irq_snapshot.take()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SdioInitState {
    ResetHost,
    PollResetHost,
    PowerOn,
    PollPowerOn,
    PostPowerOnDelay,
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
    WaitAcmd41Retry,
    PollMmcInitial,
    PollMmcReady,
    WaitMmcRetry,
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
    PollMmcDefaultClock,
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
    Failed,
}

#[derive(Debug, Clone, Copy)]
pub(super) enum SdAccessMode {
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
    fn poll_init_host_command<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<CommandResponsePoll, Error> {
        let progress = match request.take_irq_snapshot() {
            Some(snapshot) => self.host.poll_command_response_with_snapshot(snapshot),
            None => self.host.poll_command_response_at(now_ns),
        };
        request.transaction_wake_at_ns = match &progress {
            Ok(CommandResponsePoll::Pending) => self.host.command_wake_at(),
            Ok(CommandResponsePoll::Complete(_)) | Err(_) => None,
        };
        progress
    }

    fn poll_init_command_request<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<CommandResponsePoll, Error> {
        let mut command = request
            .command_request
            .take()
            .ok_or(Error::InvalidArgument)?;
        let progress = match request.take_irq_snapshot() {
            Some(snapshot) => self.poll_command_request_with_snapshot(&mut command, snapshot),
            None => self.poll_command_request_at(&mut command, now_ns),
        };
        request.transaction_wake_at_ns = match &progress {
            Ok(CommandResponsePoll::Pending) => self.command_wake_at(),
            Ok(CommandResponsePoll::Complete(_)) | Err(_) => None,
        };
        request.command_request = Some(command);
        progress
    }

    fn poll_init_status<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<CardState>, Error> {
        let mut status = request
            .status_request
            .take()
            .ok_or(Error::InvalidArgument)?;
        let progress = match request.take_irq_snapshot() {
            Some(snapshot) => self.poll_status_request_with_snapshot(&mut status, snapshot),
            None => self.poll_status_request_at(&mut status, now_ns),
        };
        request.transaction_wake_at_ns = match &progress {
            Ok(OperationPoll::Pending) => self.command_wake_at(),
            Ok(OperationPoll::Complete(_)) | Err(_) => None,
        };
        request.status_request = Some(status);
        progress
    }

    fn poll_init_ext_csd<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<()>, Error>
    where
        H: 'a,
    {
        let mut ext_csd = request
            .ext_csd_request
            .take()
            .ok_or(Error::InvalidArgument)?;
        let progress = match request.take_irq_snapshot() {
            Some(snapshot) => self.poll_ext_csd_request_with_snapshot(&mut ext_csd, snapshot),
            None => self.poll_ext_csd_request_at(&mut ext_csd, now_ns),
        };
        request.transaction_wake_at_ns = match &progress {
            Ok(OperationPoll::Pending) => self.ext_csd_request_wake_at(&ext_csd),
            Ok(OperationPoll::Complete(())) | Err(_) => None,
        };
        request.ext_csd_request = Some(ext_csd);
        progress
    }

    fn poll_init_switch_function<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<()>, Error>
    where
        H: 'a,
    {
        let mut switch = request
            .switch_function_request
            .take()
            .ok_or(Error::InvalidArgument)?;
        let progress = match request.take_irq_snapshot() {
            Some(snapshot) => {
                self.poll_switch_function_request_with_snapshot(&mut switch, snapshot)
            }
            None => self.poll_switch_function_request_at(&mut switch, now_ns),
        };
        request.transaction_wake_at_ns = match &progress {
            Ok(OperationPoll::Pending) => self.switch_function_request_wake_at(&switch),
            Ok(OperationPoll::Complete(())) | Err(_) => None,
        };
        request.switch_function_request = Some(switch);
        progress
    }

    fn poll_init_mmc_switch<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<()>, Error> {
        let mut switch = request
            .mmc_switch_request
            .take()
            .ok_or(Error::InvalidArgument)?;
        let progress = match request.take_irq_snapshot() {
            Some(snapshot) => {
                self.poll_mmc_switch_request_with_snapshot(&mut switch, now_ns, snapshot)
            }
            None => self.poll_mmc_switch_request(&mut switch, now_ns),
        };
        request.transaction_wake_at_ns = match &progress {
            Ok(OperationPoll::Pending) => self.command_wake_at(),
            Ok(OperationPoll::Complete(())) | Err(_) => None,
        };
        request.mmc_switch_request = Some(switch);
        progress
    }

    /// Submit SD/MMC card initialization without touching hardware.
    ///
    /// The caller subsequently drives the request with
    /// [`Self::poll_init_request`]. Every pending result names its next IRQ or
    /// absolute monotonic activation; invoking the request more often does
    /// not consume retries or inspect controller completion state.
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
        request.bus_wake_at_ns = None;
        request.active_bus_op = Some(op);
        request.state = next;
        Ok(OperationPoll::Pending)
    }

    fn poll_init_bus_op<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<()>, Error> {
        let mut bus_request = request.bus_request.take().ok_or(Error::InvalidArgument)?;
        match self.host.poll_bus_op_at(&mut bus_request, now_ns) {
            Ok(OperationPoll::Pending) => {
                request.bus_wake_at_ns = self.host.bus_op_wake_at(&bus_request);
                request.bus_request = Some(bus_request);
                Ok(OperationPoll::Pending)
            }
            Ok(OperationPoll::Complete(())) => {
                request.active_bus_op = None;
                request.bus_wake_at_ns = None;
                Ok(OperationPoll::Complete(()))
            }
            Err(err) => {
                warn!(
                    "sdio: init bus op {:?} failed in state {:?}: {:?}",
                    request.active_bus_op, request.state, err
                );
                request.active_bus_op = None;
                request.bus_wake_at_ns = None;
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
        request.bus_wake_at_ns = None;
        request.active_bus_op = Some(op);
        request.state = next;
        Ok(())
    }

    fn poll_init_bus_op_then<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
        complete: impl FnOnce(
            &mut Self,
            &mut SdioInitRequest<'a, H>,
        ) -> Result<OperationPoll<CardInfo>, Error>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match self.poll_init_bus_op(request, now_ns)? {
            OperationPoll::Pending => Ok(OperationPoll::Pending),
            OperationPoll::Complete(()) => complete(self, request),
        }
    }

    /// Advance one bounded initialization transition.
    ///
    /// Commands and data phases advance only after a controller IRQ. Their
    /// absolute deadline is a watchdog: reaching it without an IRQ fails the
    /// request without probing completion registers. Eventless reset, clock,
    /// power, and tuning phases use the returned absolute timer activation.
    /// A terminal failure leaves the controller unpublished and requires the
    /// owner to run its recovery/reinitialization lifecycle.
    pub fn poll_init_request<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        input: InitInput,
    ) -> InitPoll<CardInfo> {
        if request
            .last_now_ns
            .is_some_and(|previous| input.now_ns < previous)
        {
            return self.fail_init(request, Error::InvalidArgument);
        }
        request.last_now_ns = Some(input.now_ns);

        if let Some(error) = request.terminal_error {
            return InitPoll::Failed(error);
        }

        if let Some(activation) = request.pending_activation {
            match activation.activation(input) {
                Activation::Waiting => return InitPoll::Pending(activation.schedule),
                Activation::Timeout => {
                    return self.fail_init(request, Error::Timeout(ErrorContext::new(Phase::Init)));
                }
                Activation::Advance => request.pending_activation = None,
            }
        }

        request.irq_snapshot = input.snapshot;

        let progress = self.poll_init_inner(request, input.now_ns);
        if request
            .irq_snapshot
            .take()
            .is_some_and(|snapshot| snapshot.queue_service)
        {
            return self.fail_init(request, Error::InvalidArgument);
        }
        match progress {
            Ok(OperationPoll::Complete(info)) => InitPoll::Ready(info),
            Ok(OperationPoll::Pending) => {
                let activation = next_init_activation(request, input.now_ns);
                request.pending_activation = Some(activation);
                InitPoll::Pending(activation.schedule)
            }
            Err(error) => self.fail_init(request, error),
        }
    }

    #[cfg(test)]
    pub(super) fn poll_init_request_for_test<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        let now_ns = request.last_now_ns.unwrap_or(0);
        let input = match request.pending_activation.map(|wait| wait.schedule) {
            Some(schedule)
                if matches!(schedule.irq, super::init_schedule::InitIrqWait::Controller) =>
            {
                InitInput::with_controller_irq(now_ns.saturating_add(1))
            }
            Some(schedule) => InitInput::at(schedule.wake_at_ns.unwrap_or(now_ns)),
            None => InitInput::at(now_ns),
        };
        match self.poll_init_request(request, input) {
            InitPoll::Ready(info) => Ok(OperationPoll::Complete(info)),
            InitPoll::Pending(_) => Ok(OperationPoll::Pending),
            InitPoll::Failed(error) => Err(error),
        }
    }

    fn fail_init<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        error: Error,
    ) -> InitPoll<CardInfo> {
        warn!(
            "sdio: init failed closed in state {:?}: {:?}",
            request.state, error
        );
        request.state = SdioInitState::Failed;
        request.pending_activation = None;
        request.transaction_wake_at_ns = None;
        request.terminal_error = Some(error);
        self.clear_cached_card_state();
        InitPoll::Failed(error)
    }

    fn clear_cached_card_state(&mut self) {
        self.rca = 0;
        self.high_capacity = false;
        self.bus_width = BusWidth::Bit1;
        self.clock = ClockSpeed::Identification;
        self.kind = CardKind::Sd;
    }

    fn poll_init_inner<'a>(
        &mut self,
        request: &mut SdioInitRequest<'a, H>,
        now_ns: u64,
    ) -> Result<OperationPoll<CardInfo>, Error> {
        match request.state {
            SdioInitState::ResetHost
            | SdioInitState::PollResetHost
            | SdioInitState::PowerOn
            | SdioInitState::PollPowerOn
            | SdioInitState::PostPowerOnDelay
            | SdioInitState::ResetVoltage
            | SdioInitState::PollResetVoltage
            | SdioInitState::ResetBusWidth
            | SdioInitState::ResetClock
            | SdioInitState::PostIdentificationClockDelay
            | SdioInitState::SubmitCmd0
            | SdioInitState::PollCmd0 => self.poll_bootstrap_state(request, now_ns),
            SdioInitState::PollCmd8
            | SdioInitState::PollAcmd41Cmd55
            | SdioInitState::PollAcmd41
            | SdioInitState::WaitAcmd41Retry
            | SdioInitState::PollMmcInitial
            | SdioInitState::PollMmcReady
            | SdioInitState::WaitMmcRetry
            | SdioInitState::PollCmd2
            | SdioInitState::PollCmd3
            | SdioInitState::PollCmd9
            | SdioInitState::PollCmd7 => self.poll_discovery_state(request, now_ns),
            SdioInitState::PollSdBusWidthCmd55
            | SdioInitState::PollSdBusWidthAcmd6
            | SdioInitState::PollSdHostBusWidth
            | SdioInitState::FinishCardSetup
            | SdioInitState::PollSdDefaultClock
            | SdioInitState::PollMmcExtCsd
            | SdioInitState::PollMmcBusWidth
            | SdioInitState::PollMmcHostBusWidth
            | SdioInitState::PollMmcDefaultClock
            | SdioInitState::PrepareMmcSpeed => self.poll_card_setup_state(request, now_ns),
            SdioInitState::PollMmcHs200VoltageSwitch
            | SdioInitState::PollMmcHs200Switch
            | SdioInitState::PollMmcHs200Clock
            | SdioInitState::PollMmcHs200Tuning
            | SdioInitState::PollMmcHs200Status
            | SdioInitState::PollMmcHs52Switch
            | SdioInitState::PollMmcHighSpeedClock => self.poll_mmc_speed_state(request, now_ns),
            SdioInitState::PrepareSdSpeed
            | SdioInitState::PollSdSwitchFunctionCheck
            | SdioInitState::PollSdVoltageSwitch
            | SdioInitState::PollSdSignalVoltage
            | SdioInitState::PollSdSetAccessMode
            | SdioInitState::PollSdClock
            | SdioInitState::PollSdTuning
            | SdioInitState::PollSdStatus => self.poll_sd_speed_state(request, now_ns),
            SdioInitState::Complete | SdioInitState::Failed => self.poll_terminal_state(request),
        }
    }
}
