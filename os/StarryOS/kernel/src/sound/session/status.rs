use alsa_pcm_uapi::{
    LinuxTimespec, PcmMmapStatus, PcmStatus, PcmSyncPtr, SNDRV_PCM_SYNC_PTR_APPL,
    SNDRV_PCM_SYNC_PTR_AVAIL_MIN, SNDRV_PCM_SYNC_PTR_HWSYNC,
};
use axpoll::IoEvents;
use rdif_audio::PcmDirection;

use super::{PcmSession, SessionState, StreamPhase, check_error, ptr_distance, session_timespec};
use crate::sound::SoundError;

const SYNC_PTR_KNOWN_FLAGS: u32 =
    SNDRV_PCM_SYNC_PTR_HWSYNC | SNDRV_PCM_SYNC_PTR_APPL | SNDRV_PCM_SYNC_PTR_AVAIL_MIN;

impl PcmSession {
    pub(crate) fn status(&self) -> PcmStatus {
        let mut state = self.state.lock();
        let (delay, avail) = pointer_metrics(&state, self.direction);
        state.avail_max = state.avail_max.max(avail);
        let avail_max = state.avail_max;
        state.avail_max = avail;
        let mut status = PcmStatus::default();
        status.state = state.phase.status();
        status.trigger_tstamp = state.trigger_tstamp;
        status.tstamp = state.tstamp;
        status.appl_ptr = state.appl_ptr;
        status.hw_ptr = state.hw_ptr;
        status.delay = delay;
        status.avail = avail;
        status.avail_max = avail_max;
        status.overrange = state.overrange;
        status.audio_tstamp = state.tstamp;
        status.driver_tstamp = state.tstamp;
        status
    }

    pub(crate) fn delay(&self) -> i64 {
        let state = self.state.lock();
        pointer_metrics(&state, self.direction).0
    }

    pub(crate) fn update_sync_ptr(
        &self,
        mut request: PcmSyncPtr,
    ) -> Result<PcmSyncPtr, SoundError> {
        if request.flags & !SYNC_PTR_KNOWN_FLAGS != 0 {
            return Err(SoundError::InvalidInput);
        }
        {
            let _io = self.lock_io();
            let mut state = self.state.lock();
            check_error(&state)?;
            if request.flags & SNDRV_PCM_SYNC_PTR_HWSYNC != 0 {
                state.tstamp = session_timespec(&state);
            }
            if request.flags & SNDRV_PCM_SYNC_PTR_APPL == 0 {
                apply_appl_ptr(&mut state, self.direction, request.c.control.appl_ptr)?;
            }
            if request.flags & SNDRV_PCM_SYNC_PTR_AVAIL_MIN == 0 {
                if request.c.control.avail_min == 0 {
                    return Err(SoundError::InvalidInput);
                }
                state.sw.avail_min = request.c.control.avail_min;
            }
            let (_, avail) = pointer_metrics(&state, self.direction);
            state.avail_max = state.avail_max.max(avail);
            request.s.status = PcmMmapStatus {
                state: state.phase.status(),
                hw_ptr: state.hw_ptr,
                tstamp: state.tstamp,
                audio_tstamp: LinuxTimespec::default(),
                ..PcmMmapStatus::default()
            };
            request.c.control.appl_ptr = state.appl_ptr;
            request.c.control.avail_min = state.sw.avail_min;
        }
        self.notify_worker();
        self.notify();
        Ok(request)
    }

    pub(crate) fn poll_events(&self) -> IoEvents {
        let state = self.state.lock();
        if state.error.is_some() || state.phase == StreamPhase::Xrun {
            return IoEvents::ERR;
        }
        let (_, avail) = pointer_metrics(&state, self.direction);
        match self.direction {
            PcmDirection::Playback => {
                if matches!(state.phase, StreamPhase::Prepared | StreamPhase::Running)
                    && avail >= state.sw.avail_min
                    && state.format.is_some()
                {
                    IoEvents::OUT
                } else {
                    IoEvents::empty()
                }
            }
            PcmDirection::Capture => {
                if (state.phase == StreamPhase::Draining && avail > 0)
                    || (state.phase == StreamPhase::Running && avail >= state.sw.avail_min)
                {
                    IoEvents::IN
                } else {
                    IoEvents::empty()
                }
            }
        }
    }
}

fn pointer_metrics(state: &SessionState, direction: PcmDirection) -> (i64, u64) {
    let buffer_frames = state.format.map_or(0, |format| format.buffer_frames as u64);
    match direction {
        PcmDirection::Playback => {
            let delay = ptr_distance(state.hw_ptr, state.appl_ptr, state.boundary);
            (delay as i64, buffer_frames.saturating_sub(delay))
        }
        PcmDirection::Capture => {
            let delay = ptr_distance(state.appl_ptr, state.hw_ptr, state.boundary);
            (delay as i64, delay)
        }
    }
}

fn apply_appl_ptr(
    state: &mut SessionState,
    direction: PcmDirection,
    appl_ptr: u64,
) -> Result<(), SoundError> {
    if state.boundary == 0 || appl_ptr >= state.boundary {
        return Err(SoundError::InvalidInput);
    }
    let Some(format) = state.format else {
        // ALSA synchronizes the fallback control block immediately after
        // open, before HW_PARAMS establishes ring geometry.
        state.appl_ptr = appl_ptr;
        return Ok(());
    };
    let buffer_frames = u64::from(format.buffer_frames);
    let distance = match direction {
        PcmDirection::Playback => ptr_distance(state.hw_ptr, appl_ptr, state.boundary),
        PcmDirection::Capture => ptr_distance(appl_ptr, state.hw_ptr, state.boundary),
    };
    if distance > buffer_frames {
        return Err(SoundError::InvalidInput);
    }
    state.appl_ptr = appl_ptr;
    if direction == PcmDirection::Playback
        && state.phase == StreamPhase::Prepared
        && distance > 0
        && (state.sw.start_threshold == 0 || distance >= state.sw.start_threshold)
    {
        state.start_requested = true;
    }
    Ok(())
}
