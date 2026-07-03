//! SD/MMC card initialization state machine.

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
    /// Wall-time the protocol layer **assumes** elapses between two
    /// successive `poll_init_request` invocations. Document-only — the
    /// protocol code itself never multiplies anything by this constant.
    /// Caller glue should pace polls at approximately this cadence.
    pub(super) const POLL_TICK_MS_HINT: u32 = 10;

    /// Maximum number of `poll_init_request` iterations the protocol layer
    /// will tolerate while waiting for ACMD41 (SD) or CMD1 (MMC) to report
    /// `card_powered_up`. At the [`Self::POLL_TICK_MS_HINT`] cadence this is
    /// equivalent to ~1 second.
    pub(super) const MAX_POLLS: u32 = 100;

    /// Wall-clock budget for ACMD41 / CMD1 power-up retries, enforced when
    /// the host implements [`SdioHost::now_ms`]. Matches the SD spec's
    /// recommended 1 s ACMD41 retry window (sect. 4.2.3).
    pub(super) const TIMEOUT_MS: u64 = 1_000;
}

pub(super) struct MmcSwitchTiming;

impl MmcSwitchTiming {
    /// Maximum number of poll iterations spent waiting for an MMC
    /// `CMD6 SWITCH` to leave the Programming state. At the
    /// [`SdioInitTiming::POLL_TICK_MS_HINT`] cadence this is equivalent to
    /// ~250 ms — long enough to absorb worst-case `GENERIC_CMD6_TIME` while
    /// short enough that a hung card surfaces as `Error::Timeout` rather
    /// than blocking init forever.
    pub(super) const MAX_POLLS: u32 = 25;

    /// Wall-clock budget for the MMC `CMD6 SWITCH` busy-wait, enforced when
    /// the host implements [`SdioHost::now_ms`]. Sized to match `MAX_POLLS`
    /// at the recommended poll cadence so clock-aware and poll-only hosts
    /// see the same effective budget.
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

pub(super) fn sd_acmd6_arg(width: BusWidth) -> Result<u32, Error> {
    match width {
        BusWidth::Bit1 => Ok(0),
        BusWidth::Bit4 => Ok(2),
        BusWidth::Bit8 => Err(Error::UnsupportedCommand),
        _ => Err(Error::UnsupportedCommand),
    }
}
