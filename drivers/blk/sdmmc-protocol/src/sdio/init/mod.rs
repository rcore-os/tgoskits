//! SD/MMC card initialization state machine.

use core::num::NonZeroUsize;

use dma_api::{CpuDmaBuffer, DeviceDma, DmaDirection, DmaError};
use log::{debug, info, warn};

use super::{
    host::{BusWidth, ClockSpeed, SdMmcBusOp, SdMmcIrqHost, SignalVoltage},
    native::{
        CardInfo, CardKind, ExtCsdRequest, SdMmcCard, SdMmcCommandRequest, SdMmcStatusRequest,
        SwitchFunctionRequest,
    },
    transport::ProtocolBusRequest,
};
use crate::{
    block::{CommandResponseProgress, OperationProgress},
    cmd::Command,
    error::{Error, ErrorContext, Phase},
    response::{
        CardState, CidResponse, CsdResponse, OcrResponse, Response, ResponseType, SwitchStatus,
    },
};

pub(super) struct SdMmcInitTiming;

impl SdMmcInitTiming {
    /// Recommended delay between card power-up retry commands.
    ///
    /// This is a task-context register deadline hint, not a completion
    /// retry interval. Command completion still has to arrive through the
    /// IRQ path before the state machine can issue the next retry.
    pub(super) const POLL_TICK_MS_HINT: u32 = 10;

    /// Maximum number of IRQ-completed ACMD41 (SD) or CMD1 (MMC) responses
    /// that may report `card_powered_up == false`. At the
    /// [`Self::POLL_TICK_MS_HINT`] retry cadence this is equivalent to about
    /// one second.
    pub(super) const MAX_POLLS: u32 = 100;

    /// Wall-clock budget for ACMD41 / CMD1 power-up retries, enforced when
    /// the host implements [`SdMmcIrqHost::now_ms`]. Matches the SD spec's
    /// recommended 1 s ACMD41 retry window (sect. 4.2.3).
    pub(super) const TIMEOUT_MS: u64 = 1_000;
}

pub(super) struct MmcSwitchTiming;

impl MmcSwitchTiming {
    /// Maximum number of IRQ-completed status checks spent waiting for an
    /// MMC `CMD6 SWITCH` to leave the Programming state. At the
    /// [`SdMmcInitTiming::POLL_TICK_MS_HINT`] cadence this is equivalent to
    /// ~250 ms — long enough to absorb worst-case `GENERIC_CMD6_TIME` while
    /// short enough that a hung card surfaces as `Error::Timeout` rather
    /// than blocking init forever.
    pub(super) const MAX_POLLS: u32 = 25;

    /// Wall-clock budget for the MMC `CMD6 SWITCH` busy-wait, enforced when
    /// the host implements [`SdMmcIrqHost::now_ms`]. Sized to match `MAX_POLLS`
    /// at the recommended retry cadence so clock-aware and counter-only
    /// hosts see the same effective budget.
    pub(super) const TIMEOUT_MS: u64 = 250;
}

/// Return whether the wall-clock budget for ACMD41 / CMD1 power-up has
/// elapsed. `started_ms` is the time the busy-wait phase began (captured
/// from [`SdMmcIrqHost::now_ms`] on the first not-ready response). The check is
/// a no-op when either the host has no clock or the budget has not been
/// armed yet.
fn power_up_deadline_passed<H: SdMmcIrqHost>(host: &H, started_ms: Option<u64>) -> bool {
    match (started_ms, host.now_ms()) {
        (Some(started), Some(now)) => now.saturating_sub(started) >= SdMmcInitTiming::TIMEOUT_MS,
        _ => false,
    }
}

/// Return whether the wall-clock budget for MMC `CMD6 SWITCH` has elapsed.
/// See [`power_up_deadline_passed`] for the contract.
pub(super) fn mmc_switch_deadline_passed<H: SdMmcIrqHost>(
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
    /// Wall-clock submit time captured from [`SdMmcIrqHost::now_ms`], used as
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

/// CPU-owned DMA scratch used by SD/MMC initialization data commands.
pub struct SdMmcInitScratch {
    ext_csd: CpuDmaBuffer,
    switch_status: CpuDmaBuffer,
}

impl SdMmcInitScratch {
    pub fn new(device: &DeviceDma) -> Result<Self, Error> {
        Ok(Self {
            ext_csd: CpuDmaBuffer::new_zero(
                device,
                NonZeroUsize::new(512).expect("EXT_CSD size is non-zero"),
                4,
                DmaDirection::FromDevice,
            )
            .map_err(map_init_dma_error)?,
            switch_status: CpuDmaBuffer::new_zero(
                device,
                NonZeroUsize::new(64).expect("switch status size is non-zero"),
                4,
                DmaDirection::FromDevice,
            )
            .map_err(map_init_dma_error)?,
        })
    }
}

fn map_init_dma_error(error: DmaError) -> Error {
    match error {
        DmaError::AlignMismatch { .. } | DmaError::BoundaryCross { .. } => Error::Misaligned,
        DmaError::ZeroSizedBuffer | DmaError::LayoutError(_) => Error::InvalidArgument,
        DmaError::NoMemory
        | DmaError::CoherentReleaseFailed
        | DmaError::DmaMaskNotMatch { .. }
        | DmaError::SegmentTooLarge { .. }
        | DmaError::NullPointer => Error::BusError(ErrorContext::new(Phase::Init)),
    }
}

/// Submitted SDIO initialization transaction.
pub struct SdMmcInitRequest<H: SdMmcIrqHost + 'static> {
    pub(super) state: SdMmcInitState,
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
    /// [`SdMmcInitTiming::TIMEOUT_MS`] to surface an accurate timeout when
    /// the host implements [`SdMmcIrqHost::now_ms`].
    pub(super) acmd41_started_ms: Option<u64>,
    /// MMC counterpart to `acmd41_started_ms`, captured on the first CMD1
    /// not-ready response.
    pub(super) mmc_started_ms: Option<u64>,
    pub(super) mmc_ocr_arg: u32,
    pub(super) needs_pace: bool,
    pub(super) ext_csd_buf: Option<CpuDmaBuffer>,
    pub(super) switch_status_buf: Option<CpuDmaBuffer>,
    pub(super) ext_csd_request: Option<ExtCsdRequest<'static, H>>,
    pub(super) switch_function_request: Option<SwitchFunctionRequest<'static, H>>,
    pub(super) mmc_switch_request: Option<MmcSwitchRequest>,
    pub(super) status_request: Option<SdMmcStatusRequest>,
    pub(super) command_request: Option<SdMmcCommandRequest>,
    pub(super) bus_request: Option<ProtocolBusRequest<H>>,
    pub(super) active_bus_op: Option<SdMmcBusOp>,
    pub(super) current_bus_width: BusWidth,
    pub(super) current_access_mode: Option<SdAccessMode>,
    pub(super) sd_access_index: usize,
    pub(super) mmc_hs200_attempted: bool,
}

/// Runtime condition required before an initialization request may advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SdMmcInitWait {
    /// Only register state or protocol bookkeeping remains; the owner may
    /// retry in task context under its unified initialization deadline.
    Register,
    /// A command, data transfer, or tuning transaction is in flight. The
    /// owner must not call `advance_init_request` again until a device IRQ was
    /// acknowledged.
    Irq,
}

impl<H: SdMmcIrqHost + 'static> SdMmcInitRequest<H> {
    pub(super) fn new(preference: CardInitPreference, scratch: SdMmcInitScratch) -> Self {
        Self {
            state: SdMmcInitState::ResetHost,
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
            ext_csd_buf: Some(scratch.ext_csd),
            switch_status_buf: Some(scratch.switch_status),
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
    pub const fn wait_kind(&self) -> SdMmcInitWait {
        match self.state {
            SdMmcInitState::ResetHost
            | SdMmcInitState::PollResetHost
            | SdMmcInitState::PowerOn
            | SdMmcInitState::PollPowerOn
            | SdMmcInitState::ResetVoltage
            | SdMmcInitState::PollResetVoltage
            | SdMmcInitState::ResetBusWidth
            | SdMmcInitState::ResetClock
            | SdMmcInitState::PostIdentificationClockDelay
            | SdMmcInitState::SubmitCmd0
            | SdMmcInitState::SubmitAcmd41Retry
            | SdMmcInitState::SubmitMmcReadyRetry
            | SdMmcInitState::PollSdHostBusWidth
            | SdMmcInitState::FinishCardSetup
            | SdMmcInitState::PollSdDefaultClock
            | SdMmcInitState::PollMmcHostBusWidth
            | SdMmcInitState::PrepareMmcSpeed
            | SdMmcInitState::PollMmcHs200VoltageSwitch
            | SdMmcInitState::PollMmcHs200Clock
            | SdMmcInitState::PollMmcHs200RollbackVoltage
            | SdMmcInitState::PollMmcHs200RollbackClock
            | SdMmcInitState::PollMmcHighSpeedClock
            | SdMmcInitState::PrepareSdSpeed
            | SdMmcInitState::PollSdSignalVoltage
            | SdMmcInitState::PollSdClock
            | SdMmcInitState::Complete => SdMmcInitWait::Register,
            _ => SdMmcInitWait::Irq,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SdMmcInitState {
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
    SubmitAcmd41Retry,
    PollMmcInitial,
    PollMmcReady,
    SubmitMmcReadyRetry,
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
    PollMmcHs200RollbackVoltage,
    PollMmcHs200RollbackClock,
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
    }
}

mod state_machine;
