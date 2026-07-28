//! SD/MMC card initialization state machine.

use alloc::boxed::Box;

use log::{debug, info, warn};

use super::{
    card::{
        CardInfo, CardKind, ExtCsdRequest, SdioCommandRequest, SdioSdmmc, SdioStatusRequest,
        SwitchFunctionRequest,
    },
    host::{BusWidth, ClockSpeed, SdioBusOp, SdioHost, SignalVoltage},
};
use crate::{
    block::{CommandResponsePoll, OperationPoll},
    cmd::Command,
    error::{Error, ErrorContext, Phase},
    response::{
        CardState, CidResponse, CsdResponse, OcrResponse, Response, ResponseType, SwitchStatus,
    },
};

pub(super) struct SdioInitTiming;

impl SdioInitTiming {
    /// Recommended delay between card power-up retry commands.
    ///
    /// This is a task-context register deadline hint, not a completion
    /// polling interval. Command completion still has to arrive through the
    /// IRQ path before the state machine can issue the next retry.
    pub(super) const POLL_TICK_MS_HINT: u32 = 10;

    /// Maximum number of IRQ-completed ACMD41 (SD) or CMD1 (MMC) responses
    /// that may report `card_powered_up == false`. At the
    /// [`Self::POLL_TICK_MS_HINT`] retry cadence this is equivalent to about
    /// one second.
    pub(super) const MAX_POLLS: u32 = 100;

    /// Wall-clock budget for ACMD41 / CMD1 power-up retries, enforced when
    /// the host implements [`SdioHost::now_ms`]. Matches the SD spec's
    /// recommended 1 s ACMD41 retry window (sect. 4.2.3).
    pub(super) const TIMEOUT_MS: u64 = 1_000;
}

pub(super) struct MmcSwitchTiming;

impl MmcSwitchTiming {
    /// Maximum number of IRQ-completed status checks spent waiting for an
    /// MMC `CMD6 SWITCH` to leave the Programming state. At the
    /// [`SdioInitTiming::POLL_TICK_MS_HINT`] cadence this is equivalent to
    /// ~250 ms — long enough to absorb worst-case `GENERIC_CMD6_TIME` while
    /// short enough that a hung card surfaces as `Error::Timeout` rather
    /// than blocking init forever.
    pub(super) const MAX_POLLS: u32 = 25;

    /// Wall-clock budget for the MMC `CMD6 SWITCH` busy-wait, enforced when
    /// the host implements [`SdioHost::now_ms`]. Sized to match `MAX_POLLS`
    /// at the recommended retry cadence so clock-aware and counter-only
    /// hosts see the same effective budget.
    pub(super) const TIMEOUT_MS: u64 = 250;
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
pub(super) fn mmc_switch_deadline_passed<H: SdioHost>(
    host: &H,
    request: &MmcSwitchRequest,
) -> bool {
    let elapsed_exceeded = match (request.started_ms, host.now_ms()) {
        (Some(started), Some(now)) => now.saturating_sub(started) >= MmcSwitchTiming::TIMEOUT_MS,
        _ => false,
    };
    elapsed_exceeded || request.polls >= MmcSwitchTiming::MAX_POLLS
}

pub struct MmcSwitchRequest {
    pub(super) rca: u16,
    pub(super) index: u8,
    pub(super) value: u8,
    pub(super) polls: u32,
    /// Wall-clock submit time captured from [`SdioHost::now_ms`], used as
    /// the start of the [`MmcSwitchTiming::TIMEOUT_MS`] window. `None`
    /// means the host has no clock and only [`MmcSwitchTiming::MAX_POLLS`]
    /// gates the busy-wait.
    pub(super) started_ms: Option<u64>,
    pub(super) state: MmcSwitchRequestState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MmcSwitchRequestState {
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
    pub(super) acmd41_polls: u32,
    pub(super) mmc_polls: u32,
    /// Wall-clock time captured the first time ACMD41 reported the SD card
    /// was not yet powered up. Used together with
    /// [`SdioInitTiming::TIMEOUT_MS`] to surface an accurate timeout when
    /// the host implements [`SdioHost::now_ms`].
    pub(super) acmd41_started_ms: Option<u64>,
    /// MMC counterpart to `acmd41_started_ms`, captured on the first CMD1
    /// not-ready response.
    pub(super) mmc_started_ms: Option<u64>,
    pub(super) mmc_ocr_arg: u32,
    pub(super) needs_pace: bool,
    pub(super) ext_csd_buf: ScratchSlot<512>,
    pub(super) switch_status_buf: ScratchSlot<64>,
    pub(super) ext_csd_request: Option<ExtCsdRequest<'a, H>>,
    pub(super) switch_function_request: Option<SwitchFunctionRequest<'a, H>>,
    pub(super) mmc_switch_request: Option<MmcSwitchRequest>,
    pub(super) status_request: Option<SdioStatusRequest>,
    pub(super) command_request: Option<SdioCommandRequest>,
    pub(super) bus_request: Option<H::BusRequest>,
    pub(super) active_bus_op: Option<SdioBusOp>,
    pub(super) current_bus_width: BusWidth,
    pub(super) current_access_mode: Option<SdAccessMode>,
    pub(super) sd_access_index: usize,
    pub(super) mmc_hs200_attempted: bool,
    pub(super) _scratch: core::marker::PhantomData<&'a mut SdioInitScratch>,
}

/// Runtime condition required before an initialization request may advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdioInitWait {
    /// Only register state or protocol bookkeeping remains; the owner may
    /// retry in task context under its unified initialization deadline.
    Register,
    /// A command, data transfer, or tuning transaction is in flight. The
    /// owner must not call `poll_init_request` again until a device IRQ was
    /// acknowledged.
    Irq,
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
    /// The state machine sets this when the card answered ACMD41/CMD1 but
    /// has not completed power-up yet. Runtime glue can translate it into a
    /// a bounded timer or register retry. Ordinary command/data pending
    /// states do not set this hint and must wait for IRQ acknowledgement.
    pub fn take_needs_pace(&mut self) -> bool {
        let needs_pace = self.needs_pace;
        self.needs_pace = false;
        needs_pace
    }

    /// Returns the event that must precede the next state-machine step.
    ///
    /// This deliberately classifies controller reset, clock, voltage, and
    /// bus-width transitions as register work. Every command/data state is
    /// classified as IRQ work, including MMC switch/status transactions.
    pub const fn wait_kind(&self) -> SdioInitWait {
        match self.state {
            SdioInitState::ResetHost
            | SdioInitState::PollResetHost
            | SdioInitState::PowerOn
            | SdioInitState::PollPowerOn
            | SdioInitState::ResetVoltage
            | SdioInitState::PollResetVoltage
            | SdioInitState::ResetBusWidth
            | SdioInitState::ResetClock
            | SdioInitState::PostIdentificationClockDelay
            | SdioInitState::SubmitCmd0
            | SdioInitState::PollSdHostBusWidth
            | SdioInitState::FinishCardSetup
            | SdioInitState::PollSdDefaultClock
            | SdioInitState::PollMmcHostBusWidth
            | SdioInitState::PrepareMmcSpeed
            | SdioInitState::PollMmcHs200VoltageSwitch
            | SdioInitState::PollMmcHs200Clock
            | SdioInitState::PollMmcHighSpeedClock
            | SdioInitState::PrepareSdSpeed
            | SdioInitState::PollSdSignalVoltage
            | SdioInitState::PollSdClock
            | SdioInitState::Complete => SdioInitWait::Register,
            _ => SdioInitWait::Irq,
        }
    }
}

/// Heap-stable initialization request that owns its scratch DMA buffers.
///
/// The request field is declared before `scratch` so Rust drops every
/// in-flight host request before freeing the backing buffers it may reference.
pub struct OwnedSdioInitRequest<H: SdioHost + 'static> {
    request: SdioInitRequest<'static, H>,
    _scratch: Box<SdioInitScratch>,
}

impl<H: SdioHost + 'static> OwnedSdioInitRequest<H> {
    pub fn new(preference: CardInitPreference) -> Self {
        let mut scratch = Box::new(SdioInitScratch::new());
        let scratch_ptr = core::ptr::from_mut::<SdioInitScratch>(&mut *scratch);
        // SAFETY: `scratch` owns a stable heap allocation. The request is
        // never exposed independently, and field drop order destroys it
        // before `scratch`, so all embedded scratch pointers remain valid.
        let scratch_ref = unsafe { &mut *scratch_ptr };
        let request = SdioInitRequest::new(preference, scratch_ref);
        Self {
            request,
            _scratch: scratch,
        }
    }

    pub fn request_mut(&mut self) -> &mut SdioInitRequest<'static, H> {
        &mut self.request
    }

    pub const fn wait_kind(&self) -> SdioInitWait {
        self.request.wait_kind()
    }

    pub fn take_needs_pace(&mut self) -> bool {
        self.request.take_needs_pace()
    }
}

// SAFETY: the wrapper has exclusive ownership of the request and its
// heap-stable scratch. Host request and bus request values are the only
// driver-specific state that crosses the task boundary.
unsafe impl<H> Send for OwnedSdioInitRequest<H>
where
    H: SdioHost + Send + 'static,
    H::DataRequest<'static>: Send,
    H::BusRequest: Send,
{
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SdioInitState {
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
    PollMmcCacheEnable,
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

pub(super) fn sd_acmd6_arg(width: BusWidth) -> Result<u32, Error> {
    match width {
        BusWidth::Bit1 => Ok(0),
        BusWidth::Bit4 => Ok(2),
        BusWidth::Bit8 => Err(Error::UnsupportedCommand),
        _ => Err(Error::UnsupportedCommand),
    }
}

mod state_machine;
