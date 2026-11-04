use alloc::sync::Arc;

use rdif_audio::{
    PcmBuffer, PcmCompletion, PcmDirection, PcmError, PcmEventKind, PcmIoToken, PcmStream,
};

use super::{
    BackendState, COMPLETIONS_PER_TICK, CONTROL_TIMEOUT_NS, FatalPcmContext, InFlightControl,
    ReadyControl, Worker, WorkerAction, pump::now_ns,
};
use crate::sound::{SoundError, config, protocol::Request, session::StreamPhase};

impl Worker {
    pub(super) fn drain_completions(&mut self) {
        for _ in 0..COMPLETIONS_PER_TICK {
            let Some(completion) = self.card.take_completion() else {
                break;
            };
            match completion {
                PcmCompletion::Capabilities => self.publish_if_ready(),
                PcmCompletion::Control { token, result } => {
                    if self
                        .current
                        .as_ref()
                        .is_some_and(|current| current.token == token)
                    {
                        let current = self.current.take().expect("completion matched current");
                        self.finish_control(current, result);
                    } else if let Err(error) = result {
                        self.warn_fatal_pcm(
                            "control_completion_unmatched",
                            error,
                            FatalPcmContext {
                                control: Some(token),
                                ..FatalPcmContext::default()
                            },
                        );
                        let error = SoundError::from(error);
                        if error.is_fatal() {
                            self.fail_device(error);
                        }
                    }
                }
                PcmCompletion::Playback { token, frames } => {
                    self.finish_playback(token, frames);
                }
                PcmCompletion::Capture {
                    token,
                    buffer,
                    frames,
                    latency_bytes,
                } => self.finish_capture(token, buffer, frames, latency_bytes),
                PcmCompletion::CaptureCancelled { token } => {
                    self.finish_capture_cancelled(token);
                }
                PcmCompletion::Event(event) => match event.kind {
                    PcmEventKind::Xrun => {
                        if let Some(worker_session) = self.sessions.values().find(|session| {
                            session
                                .stream
                                .is_some_and(|stream| stream.stream_id() == event.data)
                        }) && let Some(session) = worker_session.session.upgrade()
                            && session.xrun_reporting_enabled()
                        {
                            session.mark_xrun();
                        }
                    }
                    PcmEventKind::PeriodElapsed
                    | PcmEventKind::JackConnected
                    | PcmEventKind::JackDisconnected
                    | PcmEventKind::Unknown(_) => {}
                },
                PcmCompletion::Shutdown { confirmed } => {
                    if !confirmed && !self.failed {
                        warn!("PCM service fatal root cause: stage=shutdown_unconfirmed");
                        self.fail_device(SoundError::NoDevice);
                    }
                }
                PcmCompletion::Fault(error) => {
                    self.warn_fatal_pcm("completion_fault", error, FatalPcmContext::default());
                    self.fail_device(SoundError::from(error));
                }
            }
        }
    }

    fn finish_control(&mut self, current: InFlightControl, result: Result<(), PcmError>) {
        let InFlightControl {
            token: control_token,
            control,
        } = current;
        let Some(mut worker_session) = self.sessions.remove(&control.session_id) else {
            if let Some(user) = control.user {
                self.complete_user(&user, Err(SoundError::NoDevice));
            }
            return;
        };
        let session = worker_session.session.upgrade();
        if let Err(error) = result {
            self.warn_fatal_pcm(
                "control_completion",
                error,
                FatalPcmContext {
                    session: Some(control.session_id),
                    direction: Some(worker_session.direction),
                    stream: worker_session.stream.map(PcmStream::stream_id),
                    control: Some(control_token),
                    ..FatalPcmContext::default()
                },
            );
            let error = SoundError::from(error);
            match control.action {
                WorkerAction::StopForLifecycle | WorkerAction::ReleaseForLifecycle => {
                    self.lifecycle_card_error(&mut worker_session, error, false);
                }
                WorkerAction::ConfigureForLifecycle | WorkerAction::PrepareForLifecycle => {
                    self.lifecycle_card_error(&mut worker_session, error, true);
                }
                WorkerAction::StopForDrain => {
                    worker_session.drain_stop_queued = false;
                    if let Some(user) = control.user {
                        self.complete_user(&user, Err(error));
                    }
                    self.fail_device(error);
                }
                WorkerAction::Start | WorkerAction::StartForDrain => {
                    worker_session.start_pending = false;
                    if let Some(user) = control.user {
                        self.complete_user(&user, Err(error));
                    }
                    if error.is_fatal() {
                        self.fail_device(error);
                    }
                }
                _ => {
                    if let Some(user) = control.user {
                        self.complete_user(&user, Err(error));
                    }
                    if error.is_fatal() {
                        self.fail_device(error);
                    }
                }
            }
            if session.is_none() && worker_session.lifecycle.is_none() && !self.failed {
                worker_session.closing = true;
                worker_session.lifecycle = Some(super::Lifecycle {
                    goal: super::LifecycleGoal::Close,
                    user: None,
                    error: None,
                    need_reset: false,
                    reset_done: true,
                });
            }
            self.sessions.insert(control.session_id, worker_session);
            return;
        }

        match control.action {
            WorkerAction::StopForLifecycle => {
                worker_session.backend = BackendState::Stopped;
            }
            WorkerAction::ReleaseForLifecycle => {
                worker_session.capture_hold.clear();
                worker_session.stream = None;
                worker_session.backend = BackendState::Released;
                worker_session.playback = None;
                worker_session.start_pending = false;
                worker_session.drain_stop_queued = false;
                if let Some(lifecycle) = worker_session.lifecycle.as_mut() {
                    lifecycle.reset_done = true;
                }
            }
            WorkerAction::ConfigureForLifecycle => {
                if let Some(config) = self.lifecycle_config(&worker_session) {
                    worker_session.config = Some(config);
                    worker_session.backend = BackendState::Configured;
                }
            }
            WorkerAction::PrepareForLifecycle => {
                worker_session.backend = BackendState::Prepared;
            }
            WorkerAction::Start => {
                worker_session.backend = BackendState::Running;
                worker_session.start_pending = false;
                if let Some(session) = session.as_ref() {
                    session.mark_running();
                }
                self.complete_user_opt_owned(control.user, Ok(()));
            }
            WorkerAction::StartForDrain => {
                worker_session.backend = BackendState::Running;
                worker_session.start_pending = false;
                if let Some(session) = session.as_ref() {
                    session.mark_running();
                }
                self.controls.push_front(ReadyControl {
                    session_id: control.session_id,
                    action: WorkerAction::Drain,
                    user: control.user,
                });
            }
            WorkerAction::StopForDrain => {
                worker_session.backend = BackendState::Stopped;
                worker_session.drain_stop_queued = false;
                if worker_session.direction == PcmDirection::Playback {
                    if let Some(session) = session.as_ref() {
                        session.mark_setup();
                    }
                } else if let Some(session) = session.as_ref() {
                    session.finish_capture_drain();
                }
                self.complete_user_opt_owned(control.user, Ok(()));
            }
            WorkerAction::Configure { .. }
            | WorkerAction::Prepare
            | WorkerAction::HwFree
            | WorkerAction::Drop
            | WorkerAction::Drain => {
                unreachable!("user lifecycle actions are not submitted directly")
            }
        }
        if !self.failed
            && let Err(error) = self.flush_capture_hold(&mut worker_session, session.as_ref())
        {
            self.handle_capture_recycle_error(
                control.session_id,
                worker_session,
                session.as_ref(),
                error,
            );
            return;
        }
        if session.is_none() && worker_session.lifecycle.is_none() && !self.failed {
            worker_session.closing = true;
            worker_session.lifecycle = Some(super::Lifecycle {
                goal: super::LifecycleGoal::Close,
                user: None,
                error: None,
                need_reset: false,
                reset_done: true,
            });
        }
        self.sessions.insert(control.session_id, worker_session);
    }

    fn complete_user_opt_owned(
        &self,
        request: Option<Arc<Request>>,
        result: Result<(), SoundError>,
    ) {
        if let Some(request) = request {
            self.complete_user(&request, result);
        }
    }

    fn finish_playback(&mut self, token: PcmIoToken, frames: u32) {
        let Some(id) = self.sessions.iter().find_map(|(id, worker_session)| {
            worker_session
                .playback
                .as_ref()
                .filter(|flight| flight.token == token)
                .map(|_| *id)
        }) else {
            return;
        };
        let Some(mut worker_session) = self.sessions.remove(&id) else {
            return;
        };
        let Some(flight) = worker_session.playback.take() else {
            self.sessions.insert(id, worker_session);
            return;
        };
        let Some(session) = worker_session.session.upgrade() else {
            worker_session.closing = true;
            if !self.failed {
                worker_session.lifecycle = Some(super::Lifecycle {
                    goal: super::LifecycleGoal::Close,
                    user: None,
                    error: None,
                    need_reset: false,
                    reset_done: true,
                });
            }
            self.sessions.insert(id, worker_session);
            return;
        };
        if frames < flight.real_frames {
            self.sessions.insert(id, worker_session);
            self.fail_device(SoundError::NoDevice);
            return;
        }
        let result = session.complete_playback(flight.real_frames);
        self.sessions.insert(id, worker_session);
        if let Err(error) = result {
            if error.is_fatal() {
                self.fail_device(error);
            } else {
                session.mark_xrun();
            }
        }
    }

    fn finish_capture(
        &mut self,
        token: PcmIoToken,
        buffer: PcmBuffer,
        frames: u32,
        latency_bytes: u32,
    ) {
        let Some(id) = self.sessions.iter().find_map(|(id, worker_session)| {
            (worker_session.direction == PcmDirection::Capture
                && worker_session.stream == Some(token.stream()))
            .then_some(*id)
        }) else {
            // The stream generation is already gone. Recycle still validates
            // the completed buffer owner and safely drops stale allocations.
            let _ = self.card.recycle_capture(token.stream(), buffer, 0);
            return;
        };
        let Some(mut worker_session) = self.sessions.remove(&id) else {
            let _ = self.card.recycle_capture(token.stream(), buffer, 0);
            return;
        };
        let Some(session) = worker_session.session.upgrade() else {
            worker_session.closing = true;
            if !self.failed {
                worker_session.lifecycle = Some(super::Lifecycle {
                    goal: super::LifecycleGoal::Close,
                    user: None,
                    error: None,
                    need_reset: false,
                    reset_done: true,
                });
            }
            if let Err(error) = self.hold_capture_buffer(&mut worker_session, buffer, false) {
                self.sessions.insert(id, worker_session);
                self.fail_device(error);
                return;
            }
            self.sessions.insert(id, worker_session);
            return;
        };
        if worker_session.closing {
            if let Err(error) = self.hold_capture_buffer(&mut worker_session, buffer, false) {
                self.sessions.insert(id, worker_session);
                self.fail_device(error);
                return;
            }
            if let Err(error) = self.flush_capture_hold(&mut worker_session, Some(&session)) {
                self.handle_capture_recycle_error(id, worker_session, Some(&session), error);
                return;
            }
            self.sessions.insert(id, worker_session);
            return;
        }
        let Some(frame_bytes) = session.frame_bytes() else {
            self.sessions.insert(id, worker_session);
            self.fail_device(SoundError::NoDevice);
            return;
        };
        let Some(expected) = (frames as usize).checked_mul(frame_bytes as usize) else {
            self.sessions.insert(id, worker_session);
            self.fail_device(SoundError::NoDevice);
            return;
        };
        if expected != buffer.filled_len()
            || latency_bytes > buffer.filled_len() as u32
            || buffer.direction() != PcmDirection::Capture
        {
            if !self.failed {
                warn!(
                    "PCM fatal root cause: stage=capture_validation session={:?} \
                     direction=Capture io={:?} expected={expected} filled={} \
                     latency={latency_bytes}",
                    id,
                    token,
                    buffer.filled_len()
                );
            }
            self.sessions.insert(id, worker_session);
            self.fail_device(SoundError::NoDevice);
            return;
        }

        let phase = session.phase();
        let stop_in_progress =
            worker_session.lifecycle.is_some() || worker_session.drain_stop_queued;
        let active_running = phase == StreamPhase::Running
            && worker_session.backend == BackendState::Running
            && !worker_session.start_pending
            && !stop_in_progress;
        let mut committed = false;
        if frames == 0 {
            // QEMU returns status-only buffers while stopping. Only a zero-size
            // completion in an otherwise active RUNNING stream is invalid.
            if active_running {
                session.mark_xrun();
            }
        } else if matches!(phase, StreamPhase::Running | StreamPhase::Draining)
            || (phase == StreamPhase::Prepared && worker_session.start_pending)
        {
            if let Err(error) = session.accept_capture(&buffer.as_slice()[..expected], frames) {
                let held = self.hold_capture_buffer(&mut worker_session, buffer, false);
                self.sessions.insert(id, worker_session);
                if let Err(hold_error) = held {
                    self.fail_device(hold_error);
                } else if error.is_fatal() {
                    if !self.failed {
                        warn!(
                            "PCM fatal root cause: stage=capture_accept error={error:?} \
                             session={id:?} direction=Capture io={token:?}"
                        );
                    }
                    self.fail_device(error);
                } else {
                    session.mark_xrun();
                }
                return;
            }
            committed = true;
        } else {
            // Late data from a stopped/prepared generation must not enter the
            // current ring. The buffer is retained only for safe recycling.
        }

        if let Err(error) = self.hold_capture_buffer(&mut worker_session, buffer, committed) {
            self.sessions.insert(id, worker_session);
            self.fail_device(error);
            return;
        }
        if let Err(error) = self.flush_capture_hold(&mut worker_session, Some(&session)) {
            self.handle_capture_recycle_error(id, worker_session, Some(&session), error);
            return;
        }
        self.sessions.insert(id, worker_session);
    }

    fn finish_capture_cancelled(&self, token: PcmIoToken) {
        // A cancellation means the card reclaimed the RX buffer while closing.
        // It carries no PCM data and must not fail the whole card.
        let _known_stream = self.sessions.values().any(|worker_session| {
            worker_session.direction == PcmDirection::Capture
                && worker_session.stream == Some(token.stream())
        });
    }

    pub(super) fn publish_if_ready(&mut self) {
        if self.published {
            return;
        }
        let Some(capabilities) = self.card.capabilities() else {
            return;
        };
        let playback = capabilities
            .playback
            .as_ref()
            .and_then(|capability| config::profile(capability).ok());
        let capture = capabilities
            .capture
            .as_ref()
            .and_then(|capability| config::profile(capability).ok());
        if playback.is_none() && capture.is_none() {
            self.service.publish_failed(SoundError::NoDevice);
            self.published = true;
            return;
        }
        self.service.publish_ready(playback, capture);
        self.published = true;
    }

    pub(super) fn fail_device(&mut self, error: SoundError) {
        if self.failed {
            return;
        }
        self.failed = true;
        self.service.fail(error);
        for worker_session in self.sessions.values_mut() {
            worker_session.capture_hold.clear();
            if let Some(lifecycle) = worker_session.lifecycle.take()
                && let Some(request) = lifecycle.user
            {
                request.complete(Err(error));
            }
        }
        let _ = self
            .card
            .begin_shutdown(now_ns(), now_ns().saturating_add(CONTROL_TIMEOUT_NS));
        if let Some(current) = self.current.take()
            && let Some(request) = current.control.user
        {
            self.complete_user(&request, Err(error));
        }
        while let Some(control) = self.controls.pop_front() {
            if let Some(request) = control.user {
                self.complete_user(&request, Err(error));
            }
        }
    }
}
