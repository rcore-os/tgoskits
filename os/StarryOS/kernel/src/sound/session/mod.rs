mod status;

use alloc::{
    sync::{Arc, Weak},
    vec,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

use alsa_pcm_uapi::{
    LinuxTimespec, PcmFormat, PcmSwParams, SNDRV_PCM_STATE_DISCONNECTED, SNDRV_PCM_STATE_DRAINING,
    SNDRV_PCM_STATE_OPEN, SNDRV_PCM_STATE_PREPARED, SNDRV_PCM_STATE_RUNNING, SNDRV_PCM_STATE_SETUP,
    SNDRV_PCM_STATE_XRUN, SNDRV_PCM_SW_PARAMS_PROTO_2_0_12, SNDRV_PCM_TSTAMP_ENABLE,
    SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY, SNDRV_PCM_TSTAMP_TYPE_MONOTONIC,
    SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW,
};
use axpoll::IoEvents;
use axpoll_set::PollSet;
use rdif_audio::{PcmConfig, PcmDirection};

use super::{
    DirectionProfile,
    protocol::{ControlCommand, SessionId, SoundError},
    service::SoundService,
};
use crate::{
    StarryError, StarryResult,
    sync::{Mutex, MutexGuard},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum StreamPhase {
    Open,
    Setup,
    Prepared,
    Running,
    Xrun,
    Draining,
    Failed,
}

impl StreamPhase {
    const fn status(self) -> i32 {
        match self {
            Self::Open => SNDRV_PCM_STATE_OPEN,
            Self::Setup => SNDRV_PCM_STATE_SETUP,
            Self::Prepared => SNDRV_PCM_STATE_PREPARED,
            Self::Running => SNDRV_PCM_STATE_RUNNING,
            Self::Xrun => SNDRV_PCM_STATE_XRUN,
            Self::Draining => SNDRV_PCM_STATE_DRAINING,
            Self::Failed => SNDRV_PCM_STATE_DISCONNECTED,
        }
    }
}

pub(crate) struct PlaybackPeriod {
    pub(crate) bytes: Vec<u8>,
    pub(crate) real_frames: u32,
}

pub(crate) enum CaptureAccept {
    /// Frames were copied into the capture ring.
    Data,
    /// The ring had no room; the state owner published XRUN.
    Overrun,
}

struct SessionState {
    phase: StreamPhase,
    format: Option<PcmFormat>,
    config: Option<PcmConfig>,
    sw: PcmSwParams,
    ring: Vec<u8>,
    hw_ptr: u64,
    appl_ptr: u64,
    submitted_ptr: u64,
    boundary: u64,
    avail_max: u64,
    overrange: u64,
    trigger_tstamp: LinuxTimespec,
    tstamp: LinuxTimespec,
    tstamp_type: u32,
    error: Option<SoundError>,
    start_requested: bool,
    /// Capture frames consumed by userspace that may fund one buffer re-arm.
    capture_recycle_credit_frames: u64,
}

impl SessionState {
    fn new() -> Self {
        let sw = PcmSwParams {
            tstamp_mode: SNDRV_PCM_TSTAMP_ENABLE,
            tstamp_type: SNDRV_PCM_TSTAMP_TYPE_MONOTONIC,
            period_step: 1,
            avail_min: 1,
            start_threshold: 1,
            stop_threshold: 1,
            boundary: 1,
            proto: SNDRV_PCM_SW_PARAMS_PROTO_2_0_12,
            ..PcmSwParams::default()
        };
        Self {
            phase: StreamPhase::Open,
            format: None,
            config: None,
            sw,
            ring: Vec::new(),
            hw_ptr: 0,
            appl_ptr: 0,
            submitted_ptr: 0,
            boundary: 1,
            avail_max: 0,
            overrange: 0,
            trigger_tstamp: LinuxTimespec::default(),
            tstamp_type: SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY,
            tstamp: timestamp_for(SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY),
            error: None,
            start_requested: false,
            capture_recycle_credit_frames: 0,
        }
    }
}

/// Shared per-open-file-description PCM state.
///
/// `dup` and `fork` share this object through [`crate::pseudofs::dev::audio::pcm::PcmFile`].
/// The worker only takes the state lock for pointer and ring updates; user
/// copies always happen outside it.
pub(crate) struct PcmSession {
    id: SessionId,
    direction: PcmDirection,
    profile: DirectionProfile,
    service: Weak<SoundService>,
    /// Serializes short PCM data commits with destructive state transitions.
    io: Mutex<()>,
    state: Mutex<SessionState>,
    poll: Arc<PollSet>,
    closed: AtomicBool,
}

impl PcmSession {
    pub(crate) fn new(
        id: SessionId,
        direction: PcmDirection,
        profile: DirectionProfile,
        service: Weak<SoundService>,
    ) -> Self {
        Self {
            id,
            direction,
            profile,
            service,
            io: Mutex::new(()),
            state: Mutex::new(SessionState::new()),
            poll: Arc::new(PollSet::new()),
            closed: AtomicBool::new(false),
        }
    }

    pub(crate) const fn id(&self) -> SessionId {
        self.id
    }

    pub(crate) const fn direction(&self) -> PcmDirection {
        self.direction
    }

    pub(crate) fn profile(&self) -> DirectionProfile {
        self.profile
    }

    pub(crate) fn closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    pub(crate) fn phase(&self) -> StreamPhase {
        self.state.lock().phase
    }

    pub(crate) fn lock_io(&self) -> MutexGuard<'_, ()> {
        self.io.lock()
    }

    pub(crate) fn frame_bytes(&self) -> Option<u32> {
        self.state.lock().format.map(|format| format.frame_bytes)
    }

    pub(crate) fn buffer_frames(&self) -> Option<u32> {
        self.state.lock().format.map(|format| format.buffer_frames)
    }

    pub(crate) fn format(&self) -> Option<PcmFormat> {
        self.state.lock().format
    }

    pub(crate) fn needs_start(&self) -> bool {
        let state = self.state.lock();
        state.phase == StreamPhase::Prepared && state.start_requested
    }

    pub(crate) fn request_capture_start(&self) {
        {
            let mut state = self.state.lock();
            if state.phase == StreamPhase::Prepared {
                state.start_requested = true;
            }
        }
        self.notify_worker();
    }

    pub(crate) fn backend_opened(&self) {
        {
            let mut state = self.state.lock();
            state.trigger_tstamp = session_timespec(&state);
            state.tstamp = state.trigger_tstamp;
        }
        self.notify();
    }

    pub(crate) fn mark_configured(&self, format: PcmFormat, config: PcmConfig) {
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            state.phase = StreamPhase::Setup;
            state.format = Some(format);
            state.config = Some(config);
            state.ring = vec![0u8; format.buffer_bytes as usize];
            state.hw_ptr = 0;
            state.appl_ptr = 0;
            state.submitted_ptr = 0;
            state.boundary = wide_boundary(u64::from(format.buffer_frames));
            state.sw.avail_min = u64::from(format.period_frames).max(1);
            state.sw.start_threshold = 1;
            state.sw.stop_threshold = u64::from(format.buffer_frames);
            state.sw.boundary = state.boundary;
            state.avail_max = 0;
            state.error = None;
            state.start_requested = false;
            state.capture_recycle_credit_frames = 0;
            state.trigger_tstamp = session_timespec(&state);
            state.tstamp = state.trigger_tstamp;
        }
        self.notify();
    }

    pub(crate) fn mark_prepared(&self) {
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            state.phase = StreamPhase::Prepared;
            state.ring.fill(0);
            reset_pointers(&mut state);
            state.error = None;
            state.start_requested = false;
            state.capture_recycle_credit_frames = 0;
            state.trigger_tstamp = session_timespec(&state);
            state.tstamp = state.trigger_tstamp;
        }
        self.notify();
    }

    pub(crate) fn mark_running(&self) {
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            if state.phase == StreamPhase::Prepared {
                state.phase = StreamPhase::Running;
            }
            state.start_requested = false;
            state.trigger_tstamp = session_timespec(&state);
            state.tstamp = state.trigger_tstamp;
        }
        self.notify();
    }

    pub(crate) fn mark_setup(&self) {
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            state.phase = StreamPhase::Setup;
            state.error = None;
            state.start_requested = false;
            state.capture_recycle_credit_frames = 0;
            state.trigger_tstamp = session_timespec(&state);
            state.tstamp = state.trigger_tstamp;
        }
        self.notify();
    }

    pub(crate) fn mark_open(&self) {
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            state.phase = StreamPhase::Open;
            state.format = None;
            state.config = None;
            state.ring = Vec::new();
            reset_pointers(&mut state);
            state.error = None;
            state.start_requested = false;
            state.capture_recycle_credit_frames = 0;
            state.trigger_tstamp = session_timespec(&state);
            state.tstamp = state.trigger_tstamp;
        }
        self.notify();
    }

    pub(crate) fn mark_draining(&self) {
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            if matches!(
                state.phase,
                StreamPhase::Prepared | StreamPhase::Running | StreamPhase::Draining
            ) {
                state.phase = StreamPhase::Draining;
                state.trigger_tstamp = session_timespec(&state);
            }
        }
        self.notify();
    }

    pub(crate) fn finish_capture_drain(&self) {
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            if state.phase != StreamPhase::Draining {
                return;
            }
            if capture_unread(&state) == 0 {
                state.phase = StreamPhase::Setup;
                reset_pointers(&mut state);
            }
            state.trigger_tstamp = session_timespec(&state);
            state.tstamp = state.trigger_tstamp;
        }
        self.notify();
    }

    pub(crate) fn mark_xrun(&self) {
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            if !matches!(state.phase, StreamPhase::Failed) {
                state.phase = StreamPhase::Xrun;
                state.start_requested = false;
                state.trigger_tstamp = session_timespec(&state);
                state.tstamp = state.trigger_tstamp;
            }
        }
        self.notify();
    }

    pub(crate) fn fail(&self, error: SoundError) {
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            state.phase = StreamPhase::Failed;
            state.error = Some(error);
            state.start_requested = false;
            state.trigger_tstamp = session_timespec(&state);
            state.tstamp = state.trigger_tstamp;
        }
        self.notify();
    }

    pub(crate) fn ensure_configurable(&self) -> Result<(), SoundError> {
        let state = self.state.lock();
        check_error(&state)?;
        match state.phase {
            StreamPhase::Open | StreamPhase::Setup => Ok(()),
            StreamPhase::Xrun => Ok(()),
            _ => Err(SoundError::BadState),
        }
    }

    pub(crate) fn set_tstamp_type(&self, value: i32) -> Result<(), SoundError> {
        let tstamp_type = u32::try_from(value).map_err(|_| SoundError::InvalidInput)?;
        if !matches!(
            tstamp_type,
            SNDRV_PCM_TSTAMP_TYPE_GETTIMEOFDAY | SNDRV_PCM_TSTAMP_TYPE_MONOTONIC
        ) {
            return Err(SoundError::InvalidInput);
        }
        {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            check_error(&state)?;
            state.tstamp_type = tstamp_type;
            state.sw.tstamp_type = tstamp_type;
            state.trigger_tstamp = timestamp_for(tstamp_type);
            state.tstamp = state.trigger_tstamp;
        }
        self.notify();
        Ok(())
    }

    pub(crate) fn set_sw_params(&self, params: PcmSwParams) -> Result<PcmSwParams, SoundError> {
        let canonical = {
            let _io = self.io.lock();
            let mut state = self.state.lock();
            check_error(&state)?;
            let format = state.format.ok_or(SoundError::BadState)?;
            let canonical = params
                .validate_and_canonicalize(&format, state.boundary)
                .map_err(|_| SoundError::InvalidInput)?;
            if canonical.proto >= SNDRV_PCM_SW_PARAMS_PROTO_2_0_12
                && canonical.tstamp_type == SNDRV_PCM_TSTAMP_TYPE_MONOTONIC_RAW
            {
                return Err(SoundError::InvalidInput);
            }
            state.sw = canonical;
            if canonical.proto >= SNDRV_PCM_SW_PARAMS_PROTO_2_0_12 {
                state.tstamp_type = canonical.tstamp_type;
            }
            state.avail_max = 0;
            canonical
        };
        self.notify();
        Ok(canonical)
    }

    /// Appends playback frames while the caller holds the session I/O guard.
    /// The caller publishes the worker and poll wakeups after dropping it.
    pub(crate) fn try_write(&self, bytes: &[u8]) -> Result<usize, SoundError> {
        let changed = {
            let mut state = self.state.lock();
            check_error(&state)?;
            if self.direction != PcmDirection::Playback {
                return Err(SoundError::BadState);
            }
            match state.phase {
                StreamPhase::Prepared | StreamPhase::Running => {}
                StreamPhase::Xrun => return Err(SoundError::Xrun),
                StreamPhase::Failed => return Err(SoundError::NoDevice),
                _ => return Err(SoundError::BadState),
            }
            let format = state.format.ok_or(SoundError::BadState)?;
            let frame_bytes = format.frame_bytes as usize;
            if frame_bytes == 0 || !bytes.len().is_multiple_of(frame_bytes) {
                return Err(SoundError::InvalidInput);
            }
            let requested = bytes.len() / frame_bytes;
            if requested == 0 {
                return Ok(0);
            }
            let queued = playback_queued(&state);
            let free = (format.buffer_frames as usize).saturating_sub(queued);
            if free == 0 {
                return Err(SoundError::WouldBlock);
            }
            let frames = requested.min(free) as u32;
            let byte_len = frames as usize * frame_bytes;
            let write_ptr = state.appl_ptr;
            copy_to_ring(
                &mut state.ring,
                format.buffer_frames,
                format.frame_bytes,
                write_ptr,
                &bytes[..byte_len],
            );
            state.appl_ptr = advance_ptr(state.appl_ptr, u64::from(frames), state.boundary);
            state.tstamp = session_timespec(&state);
            let queued = playback_queued(&state);
            let threshold = state.sw.start_threshold;
            if state.phase == StreamPhase::Prepared
                && queued > 0
                && (threshold == 0 || queued as u64 >= threshold)
            {
                state.start_requested = true;
            }
            frames
        };
        Ok(changed as usize)
    }

    pub(crate) fn peek_playback_period(
        &self,
        allow_partial: bool,
    ) -> Result<Option<PlaybackPeriod>, SoundError> {
        let state = self.state.lock();
        check_error(&state)?;
        let format = state.format.ok_or(SoundError::BadState)?;
        if !matches!(state.phase, StreamPhase::Running | StreamPhase::Draining) {
            return Ok(None);
        }
        let unsubmitted = ptr_distance(state.submitted_ptr, state.appl_ptr, state.boundary);
        if unsubmitted == 0 {
            return Ok(None);
        }
        let full_period = u64::from(format.period_frames);
        if unsubmitted < full_period && !allow_partial {
            return Ok(None);
        }
        let real_frames = unsubmitted.min(full_period) as u32;
        let real_bytes = real_frames as usize * format.frame_bytes as usize;
        let mut bytes = vec![0u8; format.period_bytes as usize];
        copy_from_ring(
            &state.ring,
            format.buffer_frames,
            format.frame_bytes,
            state.submitted_ptr,
            &mut bytes[..real_bytes],
        );
        Ok(Some(PlaybackPeriod { bytes, real_frames }))
    }

    pub(crate) fn commit_playback_submission(&self, real_frames: u32) -> Result<(), SoundError> {
        {
            let mut state = self.state.lock();
            let format = state.format.ok_or(SoundError::BadState)?;
            let unsubmitted = ptr_distance(state.submitted_ptr, state.appl_ptr, state.boundary);
            if u64::from(real_frames) > unsubmitted || real_frames > format.period_frames {
                return Err(SoundError::Io);
            }
            state.submitted_ptr =
                advance_ptr(state.submitted_ptr, u64::from(real_frames), state.boundary);
        }
        Ok(())
    }

    pub(crate) fn complete_playback(&self, real_frames: u32) -> Result<(), SoundError> {
        {
            let mut state = self.state.lock();
            let outstanding = ptr_distance(state.hw_ptr, state.submitted_ptr, state.boundary);
            if u64::from(real_frames) > outstanding {
                state.phase = StreamPhase::Xrun;
                state.error = Some(SoundError::Io);
                return Err(SoundError::Io);
            }
            state.hw_ptr = advance_ptr(state.hw_ptr, u64::from(real_frames), state.boundary);
            state.tstamp = session_timespec(&state);
        }
        self.notify();
        Ok(())
    }

    pub(crate) fn playback_is_idle(&self) -> bool {
        let state = self.state.lock();
        playback_queued(&state) == 0
            && ptr_distance(state.hw_ptr, state.submitted_ptr, state.boundary) == 0
    }

    pub(crate) fn xrun_reporting_enabled(&self) -> bool {
        let state = self.state.lock();
        state.format.is_some_and(|format| {
            state.sw.stop_threshold > 0
                && state.sw.stop_threshold <= u64::from(format.buffer_frames)
        })
    }

    pub(crate) fn playback_drain_complete(&self) -> bool {
        let state = self.state.lock();
        state.phase == StreamPhase::Draining && playback_is_idle_locked(&state)
    }

    pub(crate) fn accept_capture(
        &self,
        bytes: &[u8],
        frames: u32,
    ) -> Result<CaptureAccept, SoundError> {
        let accepted = {
            let mut state = self.state.lock();
            check_error(&state)?;
            let format = state.format.ok_or(SoundError::BadState)?;
            let expected = frames as usize * format.frame_bytes as usize;
            if bytes.len() < expected {
                state.phase = StreamPhase::Xrun;
                state.error = Some(SoundError::Io);
                return Err(SoundError::Io);
            }
            let unread = capture_unread(&state);
            let free = (format.buffer_frames as usize).saturating_sub(unread);
            if frames as usize > free {
                state.phase = StreamPhase::Xrun;
                state.trigger_tstamp = session_timespec(&state);
                state.tstamp = state.trigger_tstamp;
                return Ok(CaptureAccept::Overrun);
            }
            let write_ptr = state.hw_ptr;
            copy_to_ring(
                &mut state.ring,
                format.buffer_frames,
                format.frame_bytes,
                write_ptr,
                &bytes[..expected],
            );
            state.hw_ptr = advance_ptr(state.hw_ptr, u64::from(frames), state.boundary);
            state.tstamp = session_timespec(&state);
            let unread = capture_unread(&state);
            let threshold = state.sw.stop_threshold;
            let overrun = state.phase == StreamPhase::Running
                && threshold <= state.boundary
                && unread as u64 >= threshold
                && unread > 0;
            if overrun {
                state.phase = StreamPhase::Xrun;
                state.trigger_tstamp = state.tstamp;
            }
            CaptureAccept::Data
        };
        self.notify();
        Ok(accepted)
    }

    pub(crate) fn take_capture_recycle_credit(&self) -> bool {
        let mut state = self.state.lock();
        let Some(format) = state.format else {
            return false;
        };
        let period_frames = u64::from(format.period_frames);
        if period_frames == 0 || state.capture_recycle_credit_frames < period_frames {
            return false;
        }
        state.capture_recycle_credit_frames -= period_frames;
        true
    }

    pub(crate) fn capture_available(&self) -> Result<u32, SoundError> {
        let state = self.state.lock();
        check_error(&state)?;
        if state.phase == StreamPhase::Xrun {
            return Err(SoundError::Xrun);
        }
        if state.format.is_none() {
            return Err(SoundError::BadState);
        }
        Ok(capture_unread(&state) as u32)
    }

    pub(crate) fn capture_read_ready(&self) -> Result<bool, SoundError> {
        let state = self.state.lock();
        check_error(&state)?;
        match state.phase {
            StreamPhase::Xrun => Err(SoundError::Xrun),
            StreamPhase::Open | StreamPhase::Setup | StreamPhase::Failed => {
                Err(SoundError::BadState)
            }
            StreamPhase::Draining => Ok(capture_unread(&state) > 0),
            StreamPhase::Prepared | StreamPhase::Running => {
                Ok(capture_unread(&state) as u64 >= state.sw.avail_min)
            }
        }
    }

    pub(crate) fn playback_writable(&self) -> Result<bool, SoundError> {
        let state = self.state.lock();
        check_error(&state)?;
        match state.phase {
            StreamPhase::Xrun => Err(SoundError::Xrun),
            StreamPhase::Prepared | StreamPhase::Running => {
                let format = state.format.ok_or(SoundError::BadState)?;
                Ok(playback_queued(&state) < format.buffer_frames as usize)
            }
            _ => Err(SoundError::BadState),
        }
    }

    pub(crate) fn peek_capture(
        &self,
        max_bytes: usize,
    ) -> Result<Option<(Vec<u8>, u32)>, SoundError> {
        let state = self.state.lock();
        check_error(&state)?;
        if state.phase == StreamPhase::Xrun {
            return Err(SoundError::Xrun);
        }
        if !matches!(
            state.phase,
            StreamPhase::Prepared | StreamPhase::Running | StreamPhase::Draining
        ) {
            return Err(SoundError::BadState);
        }
        let format = state.format.ok_or(SoundError::BadState)?;
        let unread = capture_unread(&state);
        if unread == 0 {
            return Ok(None);
        }
        let frames = (max_bytes / format.frame_bytes as usize).min(unread) as u32;
        if frames == 0 {
            return Ok(None);
        }
        let mut bytes = vec![0u8; frames as usize * format.frame_bytes as usize];
        copy_from_ring(
            &state.ring,
            format.buffer_frames,
            format.frame_bytes,
            state.appl_ptr,
            &mut bytes,
        );
        Ok(Some((bytes, frames)))
    }

    /// Commits a completed capture copy while the caller holds the I/O guard.
    /// The caller publishes the poll wakeup after dropping it.
    pub(crate) fn commit_capture_read(&self, frames: u32) -> Result<(), SoundError> {
        {
            let mut state = self.state.lock();
            check_error(&state)?;
            let unread = capture_unread(&state);
            if frames as usize > unread {
                return Err(SoundError::Io);
            }
            state.appl_ptr = advance_ptr(state.appl_ptr, u64::from(frames), state.boundary);
            state.capture_recycle_credit_frames = state
                .capture_recycle_credit_frames
                .saturating_add(u64::from(frames));
            state.tstamp = session_timespec(&state);
            if state.phase == StreamPhase::Draining && capture_unread(&state) == 0 {
                state.phase = StreamPhase::Setup;
                reset_pointers(&mut state);
                state.trigger_tstamp = state.tstamp;
            }
        }
        Ok(())
    }

    pub(crate) fn poll_set(&self) -> &PollSet {
        self.poll.as_ref()
    }

    pub(crate) fn poll_source(&self) -> Arc<PollSet> {
        self.poll.clone()
    }

    pub(crate) fn notify(&self) {
        // SAFETY: all session mutations and PollSet users run in task context.
        // State is published under the session mutex before this wake.
        unsafe {
            self.poll.wake(IoEvents::IN | IoEvents::OUT | IoEvents::ERR);
        }
    }

    pub(crate) fn notify_worker(&self) {
        if let Some(service) = self.service.upgrade() {
            service.notify_worker();
        }
    }

    pub(crate) fn submit_control(self: Arc<Self>, command: ControlCommand) -> StarryResult<()> {
        let service = self.service.upgrade().ok_or(StarryError::NoSuchDevice)?;
        service.enqueue_control(Arc::downgrade(&self), command)
    }
}

impl Drop for PcmSession {
    fn drop(&mut self) {
        self.closed.store(true, Ordering::Release);
        if let Some(service) = self.service.upgrade() {
            service.notify_worker();
        }
    }
}

fn check_error(state: &SessionState) -> Result<(), SoundError> {
    if let Some(error) = state.error {
        return Err(error);
    }
    if state.phase == StreamPhase::Failed {
        return Err(SoundError::NoDevice);
    }
    Ok(())
}

fn reset_pointers(state: &mut SessionState) {
    state.hw_ptr = 0;
    state.appl_ptr = 0;
    state.submitted_ptr = 0;
    state.avail_max = 0;
}

fn wide_boundary(buffer_frames: u64) -> u64 {
    let mut boundary = buffer_frames.max(1);
    while let Some(next) = boundary.checked_mul(2)
        && next <= i64::MAX as u64 - buffer_frames
    {
        boundary = next;
    }
    boundary
}

fn playback_queued(state: &SessionState) -> usize {
    ptr_distance(state.hw_ptr, state.appl_ptr, state.boundary) as usize
}

fn playback_is_idle_locked(state: &SessionState) -> bool {
    playback_queued(state) == 0
        && ptr_distance(state.hw_ptr, state.submitted_ptr, state.boundary) == 0
}

fn capture_unread(state: &SessionState) -> usize {
    ptr_distance(state.appl_ptr, state.hw_ptr, state.boundary) as usize
}

fn advance_ptr(pointer: u64, frames: u64, boundary: u64) -> u64 {
    debug_assert!(boundary > 0);
    (pointer + frames) % boundary
}

fn ptr_distance(from: u64, to: u64, boundary: u64) -> u64 {
    if boundary == 0 {
        return 0;
    }
    (to + boundary - from) % boundary
}

fn copy_to_ring(
    ring: &mut [u8],
    buffer_frames: u32,
    frame_bytes: u32,
    start_ptr: u64,
    bytes: &[u8],
) {
    let frame_bytes = frame_bytes as usize;
    let capacity = buffer_frames as usize * frame_bytes;
    if ring.is_empty() || capacity == 0 || bytes.is_empty() {
        return;
    }
    let start = (start_ptr % u64::from(buffer_frames)) as usize * frame_bytes;
    let first = bytes.len().min(capacity - start);
    ring[start..start + first].copy_from_slice(&bytes[..first]);
    let remaining = bytes.len() - first;
    if remaining > 0 {
        ring[..remaining].copy_from_slice(&bytes[first..]);
    }
}

fn copy_from_ring(
    ring: &[u8],
    buffer_frames: u32,
    frame_bytes: u32,
    start_ptr: u64,
    bytes: &mut [u8],
) {
    let frame_bytes = frame_bytes as usize;
    let capacity = buffer_frames as usize * frame_bytes;
    if ring.len() < capacity || capacity == 0 || bytes.is_empty() {
        return;
    }
    let start = (start_ptr % u64::from(buffer_frames)) as usize * frame_bytes;
    let first = bytes.len().min(capacity - start);
    bytes[..first].copy_from_slice(&ring[start..start + first]);
    let remaining = bytes.len() - first;
    if remaining > 0 {
        bytes[first..].copy_from_slice(&ring[..remaining]);
    }
}

fn session_timespec(state: &SessionState) -> LinuxTimespec {
    timestamp_for(state.tstamp_type)
}

fn timestamp_for(tstamp_type: u32) -> LinuxTimespec {
    let now = match tstamp_type {
        SNDRV_PCM_TSTAMP_TYPE_MONOTONIC => ax_runtime::hal::time::monotonic_time(),
        _ => ax_runtime::hal::time::wall_time(),
    };
    LinuxTimespec {
        tv_sec: now.as_secs() as i64,
        tv_nsec: i64::from(now.subsec_nanos()),
    }
}
