use alloc::collections::VecDeque;

use rdif_audio::{PcmCard, PcmDirection, PcmError, PcmStream};

use super::{
    BackendState, CONTROL_TIMEOUT_NS, PlaybackFlight, ReadyControl, WorkerAction, WorkerSession,
};
use crate::sound::{
    SoundError,
    session::{PcmSession, StreamPhase},
};

pub(super) fn pump_session(
    card: &mut dyn PcmCard,
    worker_session: &mut WorkerSession,
    controls: &mut VecDeque<ReadyControl>,
    session: &PcmSession,
) -> Result<(), SoundError> {
    let phase = session.phase();
    if phase == StreamPhase::Failed {
        return Err(SoundError::NoDevice);
    }
    if phase == StreamPhase::Prepared
        && worker_session.backend == BackendState::Prepared
        && session.needs_start()
        && !worker_session.start_pending
    {
        worker_session.start_pending = true;
        controls.push_back(ReadyControl {
            session_id: worker_session.id,
            action: WorkerAction::Start,
            user: None,
        });
    }

    match worker_session.direction {
        PcmDirection::Playback => {
            if worker_session.backend == BackendState::Running
                && matches!(phase, StreamPhase::Running | StreamPhase::Draining)
                && worker_session.playback.is_none()
            {
                let Some(stream) = worker_session.stream else {
                    return Ok(());
                };
                let allow_partial = phase == StreamPhase::Draining;
                if let Some(period) = session.peek_playback_period(allow_partial)? {
                    match card.write_playback(stream, &period.bytes, control_deadline()) {
                        Ok(token) => {
                            session.commit_playback_submission(period.real_frames)?;
                            worker_session.playback = Some(PlaybackFlight {
                                token,
                                real_frames: period.real_frames,
                            });
                        }
                        Err(PcmError::Again) => {}
                        Err(error) => {
                            return Err(warn_card_error("playback_submit", error, worker_session));
                        }
                    }
                }
            }
            if worker_session.backend == BackendState::Running
                && phase == StreamPhase::Draining
                && worker_session.playback.is_none()
                && session.playback_drain_complete()
                && !worker_session.drain_stop_queued
            {
                worker_session.drain_stop_queued = true;
                controls.push_back(ReadyControl {
                    session_id: worker_session.id,
                    action: WorkerAction::StopForDrain,
                    user: None,
                });
            }
            if worker_session.backend == BackendState::Running
                && phase == StreamPhase::Running
                && worker_session.playback.is_none()
                && session.playback_is_idle()
                && session.xrun_reporting_enabled()
            {
                let now = now_ns();
                let idle_since = *worker_session.idle_since_ns.get_or_insert(now);
                if now.saturating_sub(idle_since) >= playback_idle_limit_ns(session) {
                    session.mark_xrun();
                }
            } else {
                worker_session.idle_since_ns = None;
            }
        }
        PcmDirection::Capture => {}
    }
    Ok(())
}

pub(super) fn now_ns() -> u64 {
    u64::try_from(ax_runtime::hal::time::monotonic_time().as_nanos()).unwrap_or(u64::MAX)
}

fn control_deadline() -> u64 {
    now_ns().saturating_add(CONTROL_TIMEOUT_NS)
}

fn warn_card_error(
    stage: &'static str,
    error: PcmError,
    worker_session: &WorkerSession,
) -> SoundError {
    let mapped = SoundError::from(error);
    if mapped.is_fatal() {
        warn!(
            "PCM fatal root cause: stage={stage} error={error:?} mapped={mapped:?} session={:?} \
             direction={:?} stream={:?}",
            worker_session.id,
            worker_session.direction,
            worker_session.stream.map(PcmStream::stream_id)
        );
    }
    mapped
}

fn playback_idle_limit_ns(session: &PcmSession) -> u64 {
    let Some(format) = session.format() else {
        return 0;
    };
    u64::from(format.period_frames)
        .saturating_mul(1_000_000_000)
        .checked_div(u64::from(format.rate_hz))
        .unwrap_or(0)
        .max(1)
}
