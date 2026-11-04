use alloc::{
    boxed::Box,
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Weak},
    vec::Vec,
};
use core::time::Duration;

mod completion;
mod lifecycle;
mod pump;

use alsa_pcm_uapi::PcmFormat;
use rdif_audio::{
    PcmBuffer, PcmCard, PcmCardState, PcmConfig, PcmControlToken, PcmDirection, PcmError,
    PcmIoToken, PcmServiceBudget, PcmStream,
};

use self::pump::{now_ns, pump_session};
use super::{
    protocol::{ControlCommand, ControlKind, Request, SessionId, SoundError},
    service::{Command, SoundService},
    session::{PcmSession, StreamPhase},
};

const COMMANDS_PER_TICK: usize = 8;
const COMPLETIONS_PER_TICK: usize = 64;
pub(super) const CONTROL_TIMEOUT_NS: u64 = 1_000_000_000;
const SERVICE_INTERVAL: Duration = Duration::from_millis(1);
const SERVICE_BUDGET: PcmServiceBudget = PcmServiceBudget::uniform(4);

pub(crate) fn run(card: Box<dyn PcmCard>, service: Arc<SoundService>) {
    Worker {
        card,
        service,
        sessions: BTreeMap::new(),
        controls: VecDeque::new(),
        current: None,
        published: false,
        failed: false,
    }
    .run();
}

struct Worker {
    card: Box<dyn PcmCard>,
    service: Arc<SoundService>,
    sessions: BTreeMap<SessionId, WorkerSession>,
    controls: VecDeque<ReadyControl>,
    current: Option<InFlightControl>,
    published: bool,
    failed: bool,
}

struct WorkerSession {
    id: SessionId,
    direction: PcmDirection,
    stream: Option<PcmStream>,
    backend: BackendState,
    config: Option<PcmConfig>,
    session: Weak<PcmSession>,
    start_pending: bool,
    closing: bool,
    lifecycle: Option<Lifecycle>,
    playback: Option<PlaybackFlight>,
    /// Completed RX buffers whose data is already copied or deliberately
    /// discarded. The worker remains their sole owner until recycle or release.
    capture_hold: VecDeque<CaptureHoldBuffer>,
    drain_stop_queued: bool,
    idle_since_ns: Option<u64>,
}

/// Backend lifecycle mirrors the card session state and is independent from
/// the user-visible ALSA state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BackendState {
    Released,
    Opened,
    Configured,
    Prepared,
    Running,
    Stopped,
}

/// One user operation that may span several card control completions.
struct Lifecycle {
    goal: LifecycleGoal,
    user: Option<Arc<Request>>,
    error: Option<SoundError>,
    need_reset: bool,
    reset_done: bool,
}

#[derive(Clone, Copy)]
enum LifecycleGoal {
    Prepare,
    Drop,
    HwFree,
    Configure {
        format: PcmFormat,
        config: PcmConfig,
    },
    CleanupOpen,
    Close,
}

struct PlaybackFlight {
    token: PcmIoToken,
    real_frames: u32,
}

struct CaptureHoldBuffer {
    buffer: PcmBuffer,
    committed: bool,
}

struct ReadyControl {
    session_id: SessionId,
    action: WorkerAction,
    user: Option<Arc<Request>>,
}

#[derive(Clone, Copy)]
enum WorkerAction {
    Configure {
        format: PcmFormat,
        config: PcmConfig,
    },
    Prepare,
    Start,
    StartForDrain,
    HwFree,
    Drop,
    Drain,
    StopForDrain,
    StopForLifecycle,
    ReleaseForLifecycle,
    ConfigureForLifecycle,
    PrepareForLifecycle,
}

struct InFlightControl {
    token: PcmControlToken,
    control: ReadyControl,
}

#[derive(Clone, Copy, Default)]
struct FatalPcmContext {
    session: Option<SessionId>,
    direction: Option<PcmDirection>,
    stream: Option<u32>,
    control: Option<PcmControlToken>,
    io: Option<PcmIoToken>,
}

enum SubmitOutcome {
    Submitted(PcmControlToken),
    Retry,
    Immediate,
}

impl Worker {
    fn run(mut self) {
        loop {
            let observed_epoch = self.service.work_epoch();
            self.step();
            self.service.wait_for_work(SERVICE_INTERVAL, observed_epoch);
        }
    }

    fn step(&mut self) {
        self.dispatch_commands();
        self.detect_closes();
        self.schedule_sessions();
        self.submit_next_control();

        let report = self.card.service(now_ns(), SERVICE_BUDGET);
        if let Some(pcm_error) = report.error {
            let error = SoundError::from(pcm_error);
            if error.is_fatal() {
                let context =
                    self.current
                        .as_ref()
                        .map_or_else(FatalPcmContext::default, |current| FatalPcmContext {
                            session: Some(current.control.session_id),
                            direction: self
                                .sessions
                                .get(&current.control.session_id)
                                .map(|session| session.direction),
                            stream: self
                                .sessions
                                .get(&current.control.session_id)
                                .and_then(|session| session.stream)
                                .map(PcmStream::stream_id),
                            control: Some(current.token),
                            ..FatalPcmContext::default()
                        });
                self.warn_fatal_pcm("card_service", pcm_error, context);
                self.fail_device(error);
            }
        }
        if matches!(report.state, PcmCardState::Failed) && !self.failed {
            warn!("PCM service fatal root cause: stage=card_state state=Failed");
            self.fail_device(SoundError::NoDevice);
        }
        self.drain_completions();
        if report.backpressure {
            self.service.notify_worker();
        }
        self.publish_if_ready();
    }

    fn warn_fatal_pcm(&self, stage: &'static str, error: PcmError, context: FatalPcmContext) {
        let mapped = SoundError::from(error);
        if !self.failed && mapped.is_fatal() {
            warn!(
                "PCM fatal root cause: stage={stage} error={error:?} mapped={mapped:?} \
                 session={:?} direction={:?} stream={:?} control={:?} io={:?}",
                context.session, context.direction, context.stream, context.control, context.io
            );
        }
    }

    fn hold_capture_buffer(
        &self,
        worker_session: &mut WorkerSession,
        buffer: PcmBuffer,
        committed: bool,
    ) -> Result<(), SoundError> {
        let limit = worker_session
            .config
            .map(|config| usize::from(config.period_count))
            .unwrap_or(0);
        if limit == 0 || worker_session.capture_hold.len() >= limit {
            return Err(SoundError::Io);
        }
        worker_session
            .capture_hold
            .try_reserve(1)
            .map_err(|_| SoundError::Io)?;
        worker_session
            .capture_hold
            .push_back(CaptureHoldBuffer { buffer, committed });
        Ok(())
    }

    fn flush_capture_hold(
        &mut self,
        worker_session: &mut WorkerSession,
        session: Option<&Arc<PcmSession>>,
    ) -> Result<(), SoundError> {
        let Some(session) = session else {
            return Ok(());
        };
        let stopped = worker_session.backend == BackendState::Stopped;
        let running = worker_session.backend == BackendState::Running
            && session.phase() == StreamPhase::Running
            && worker_session.lifecycle.is_none()
            && !worker_session.drain_stop_queued;
        if !stopped && !running {
            return Ok(());
        }
        let Some(stream) = worker_session.stream else {
            return Ok(());
        };
        loop {
            let Some(hold) = worker_session.capture_hold.pop_front() else {
                break;
            };
            if running && hold.committed && !session.take_capture_recycle_credit() {
                worker_session.capture_hold.push_front(hold);
                break;
            }
            if let Err(error) = self.card.recycle_capture(stream, hold.buffer, 0) {
                let error = SoundError::from(error);
                if error.is_fatal() || running {
                    return Err(error);
                }
                return Ok(());
            }
        }
        Ok(())
    }

    fn handle_capture_recycle_error(
        &mut self,
        id: SessionId,
        worker_session: WorkerSession,
        session: Option<&Arc<PcmSession>>,
        error: SoundError,
    ) {
        self.sessions.insert(id, worker_session);
        if error.is_fatal() {
            self.fail_device(error);
        } else if let Some(session) = session {
            session.mark_xrun();
        }
    }

    fn dispatch_commands(&mut self) {
        for _ in 0..COMMANDS_PER_TICK {
            let Some(command) = self.service.take_command() else {
                break;
            };
            match command {
                Command::Open {
                    id,
                    direction,
                    session,
                } => self.open_session(id, direction, session),
                Command::Control { session, command } => {
                    let Some(session) = session.upgrade() else {
                        self.complete_user(&command.request, Err(SoundError::NoDevice));
                        continue;
                    };
                    if self.failed || session.closed() {
                        self.complete_user(&command.request, Err(SoundError::NoDevice));
                        continue;
                    }
                    let ControlCommand {
                        kind,
                        request,
                        configure,
                    } = command;
                    let action = match kind {
                        ControlKind::Configure => {
                            let Some((format, config)) = configure else {
                                self.complete_user(&request, Err(SoundError::InvalidInput));
                                continue;
                            };
                            WorkerAction::Configure { format, config }
                        }
                        ControlKind::Prepare => WorkerAction::Prepare,
                        ControlKind::Start => WorkerAction::Start,
                        ControlKind::HwFree => WorkerAction::HwFree,
                        ControlKind::Drop => WorkerAction::Drop,
                        ControlKind::Drain => WorkerAction::Drain,
                    };
                    self.controls.push_back(ReadyControl {
                        session_id: session.id(),
                        action,
                        user: Some(request),
                    });
                }
            }
        }
    }

    fn complete_user(&self, request: &Arc<Request>, result: Result<(), SoundError>) {
        self.service.complete_control(request, result);
    }

    fn open_session(&mut self, id: SessionId, direction: PcmDirection, session: Weak<PcmSession>) {
        let Some(session) = session.upgrade() else {
            self.service.release_slot(id);
            return;
        };
        if self.failed || session.closed() {
            self.service.release_slot(id);
            return;
        }
        match self.card.open(direction) {
            Ok(stream) => {
                self.sessions.insert(
                    id,
                    WorkerSession {
                        id,
                        direction,
                        stream: Some(stream),
                        backend: BackendState::Opened,
                        config: None,
                        session: Arc::downgrade(&session),
                        start_pending: false,
                        closing: false,
                        lifecycle: None,
                        playback: None,
                        capture_hold: VecDeque::new(),
                        drain_stop_queued: false,
                        idle_since_ns: None,
                    },
                );
                session.backend_opened();
            }
            Err(error) => {
                self.warn_fatal_pcm(
                    "stream_open",
                    error,
                    FatalPcmContext {
                        session: Some(id),
                        direction: Some(direction),
                        ..FatalPcmContext::default()
                    },
                );
                let error = SoundError::from(error);
                session.fail(error);
                if error.is_fatal() {
                    self.fail_device(error);
                }
            }
        }
    }

    fn detect_closes(&mut self) {
        for (id, _direction, weak) in self.service.sessions() {
            if let Some(worker_session) = self.sessions.get_mut(&id) {
                let gone = match weak.upgrade() {
                    Some(session) => session.closed(),
                    None => true,
                };
                if !worker_session.closing && gone {
                    worker_session.closing = true;
                    if let Some(lifecycle) = worker_session.lifecycle.take()
                        && let Some(request) = lifecycle.user
                    {
                        request.complete(Err(SoundError::NoDevice));
                    }
                    worker_session.lifecycle = Some(Lifecycle {
                        goal: LifecycleGoal::Close,
                        user: None,
                        error: None,
                        need_reset: false,
                        reset_done: true,
                    });
                    self.service.notify_worker();
                }
            } else if weak.upgrade().is_none() {
                self.service.release_slot(id);
            }
        }
    }

    fn schedule_sessions(&mut self) {
        let ids: Vec<_> = self.sessions.keys().copied().collect();
        for id in ids {
            let Some(mut worker_session) = self.sessions.remove(&id) else {
                continue;
            };
            if worker_session.lifecycle.is_some() {
                let session = worker_session.session.upgrade();
                self.drive_lifecycle(&mut worker_session, session.as_ref());
                if worker_session.closing
                    && worker_session.lifecycle.is_none()
                    && worker_session.stream.is_none()
                {
                    continue;
                }
                if worker_session.lifecycle.is_some() || worker_session.closing {
                    self.sessions.insert(id, worker_session);
                    continue;
                }
            }
            if self.failed {
                self.sessions.insert(id, worker_session);
                continue;
            }
            let session = worker_session.session.upgrade();
            if let Some(session) = session.as_ref()
                && let Err(error) = pump_session(
                    &mut *self.card,
                    &mut worker_session,
                    &mut self.controls,
                    session,
                )
            {
                if error.is_fatal() {
                    self.sessions.insert(id, worker_session);
                    self.fail_device(error);
                } else {
                    self.sessions.insert(id, worker_session);
                    session.mark_xrun();
                }
                continue;
            }
            if let Err(error) = self.flush_capture_hold(&mut worker_session, session.as_ref()) {
                self.handle_capture_recycle_error(id, worker_session, session.as_ref(), error);
                continue;
            }
            self.sessions.insert(id, worker_session);
        }
    }

    fn submit_next_control(&mut self) {
        while self.current.is_none() {
            let Some(mut control) = self.controls.pop_front() else {
                return;
            };
            if self.failed
                && !matches!(
                    control.action,
                    WorkerAction::StopForLifecycle
                        | WorkerAction::ReleaseForLifecycle
                        | WorkerAction::ConfigureForLifecycle
                        | WorkerAction::PrepareForLifecycle
                        | WorkerAction::StopForDrain
                )
            {
                if let Some(user) = control.user.take() {
                    self.complete_user(&user, Err(SoundError::NoDevice));
                }
                continue;
            }
            let Some(mut worker_session) = self.sessions.remove(&control.session_id) else {
                if let Some(user) = control.user.take() {
                    self.complete_user(&user, Err(SoundError::NoDevice));
                }
                continue;
            };
            let backend_step = matches!(
                control.action,
                WorkerAction::StopForLifecycle
                    | WorkerAction::ReleaseForLifecycle
                    | WorkerAction::ConfigureForLifecycle
                    | WorkerAction::PrepareForLifecycle
                    | WorkerAction::StopForDrain
            );
            let outcome = if backend_step {
                self.submit_action(&mut control, &mut worker_session, None)
            } else {
                let Some(session) = worker_session.session.upgrade() else {
                    self.sessions.insert(control.session_id, worker_session);
                    if let Some(user) = control.user.take() {
                        self.complete_user(&user, Err(SoundError::NoDevice));
                    }
                    continue;
                };
                self.submit_action(&mut control, &mut worker_session, Some(&session))
            };
            self.sessions.insert(control.session_id, worker_session);
            match outcome {
                SubmitOutcome::Submitted(token) => {
                    self.current = Some(InFlightControl { token, control });
                    return;
                }
                SubmitOutcome::Retry => {
                    self.controls.push_front(control);
                    return;
                }
                SubmitOutcome::Immediate => {}
            }
        }
    }

    fn submit_action(
        &mut self,
        control: &mut ReadyControl,
        worker_session: &mut WorkerSession,
        session: Option<&Arc<PcmSession>>,
    ) -> SubmitOutcome {
        let now = now_ns();
        let deadline = now.saturating_add(CONTROL_TIMEOUT_NS);
        match control.action {
            WorkerAction::Configure { format, config } => {
                let Some(session) = session else {
                    self.complete_user_opt(control, Err(SoundError::NoDevice));
                    return SubmitOutcome::Immediate;
                };
                if let Err(error) = session.ensure_configurable() {
                    self.complete_user_opt(control, Err(error));
                    return SubmitOutcome::Immediate;
                }
                if worker_session.lifecycle.is_some() {
                    self.complete_user_opt(control, Err(SoundError::Busy));
                    return SubmitOutcome::Immediate;
                }
                let need_reset = worker_session.backend != BackendState::Opened;
                let reset_done = worker_session.stream.is_none()
                    || worker_session.backend == BackendState::Opened;
                worker_session.lifecycle = Some(Lifecycle {
                    goal: LifecycleGoal::Configure { format, config },
                    user: control.user.take(),
                    error: None,
                    need_reset,
                    reset_done,
                });
                SubmitOutcome::Immediate
            }
            WorkerAction::Prepare => {
                let Some(session) = session else {
                    self.complete_user_opt(control, Err(SoundError::NoDevice));
                    return SubmitOutcome::Immediate;
                };
                if matches!(session.phase(), StreamPhase::Open | StreamPhase::Failed) {
                    self.complete_user_opt(control, Err(SoundError::BadState));
                    return SubmitOutcome::Immediate;
                }
                if worker_session.lifecycle.is_some() {
                    self.complete_user_opt(control, Err(SoundError::Busy));
                    return SubmitOutcome::Immediate;
                }
                let need_reset = worker_session.backend != BackendState::Configured;
                let reset_done = worker_session.stream.is_none()
                    || worker_session.backend == BackendState::Opened;
                worker_session.lifecycle = Some(Lifecycle {
                    goal: LifecycleGoal::Prepare,
                    user: control.user.take(),
                    error: None,
                    need_reset,
                    reset_done,
                });
                SubmitOutcome::Immediate
            }
            WorkerAction::HwFree => {
                let Some(session) = session else {
                    self.complete_user_opt(control, Err(SoundError::NoDevice));
                    return SubmitOutcome::Immediate;
                };
                if session.phase() == StreamPhase::Open {
                    self.complete_user_opt(control, Ok(()));
                    return SubmitOutcome::Immediate;
                }
                if worker_session.lifecycle.is_some() {
                    self.complete_user_opt(control, Err(SoundError::Busy));
                    return SubmitOutcome::Immediate;
                }
                worker_session.lifecycle = Some(Lifecycle {
                    goal: LifecycleGoal::HwFree,
                    user: control.user.take(),
                    error: None,
                    need_reset: false,
                    reset_done: true,
                });
                SubmitOutcome::Immediate
            }
            WorkerAction::Drop => {
                let Some(session) = session else {
                    self.complete_user_opt(control, Err(SoundError::NoDevice));
                    return SubmitOutcome::Immediate;
                };
                if session.phase() == StreamPhase::Open {
                    self.complete_user_opt(control, Err(SoundError::BadState));
                    return SubmitOutcome::Immediate;
                }
                if worker_session.lifecycle.is_some() {
                    self.complete_user_opt(control, Err(SoundError::Busy));
                    return SubmitOutcome::Immediate;
                }
                let need_reset = worker_session.stream.is_some();
                let reset_done = worker_session.stream.is_none()
                    || worker_session.backend == BackendState::Opened;
                worker_session.lifecycle = Some(Lifecycle {
                    goal: LifecycleGoal::Drop,
                    user: control.user.take(),
                    error: None,
                    need_reset,
                    reset_done,
                });
                SubmitOutcome::Immediate
            }
            WorkerAction::Start | WorkerAction::StartForDrain => {
                let Some(session) = session else {
                    self.complete_user_opt(control, Err(SoundError::NoDevice));
                    return SubmitOutcome::Immediate;
                };
                if worker_session.backend == BackendState::Running {
                    worker_session.start_pending = false;
                    if matches!(control.action, WorkerAction::StartForDrain) {
                        self.queue_drain_after_start(control);
                    } else {
                        self.complete_user_opt(control, Ok(()));
                    }
                    return SubmitOutcome::Immediate;
                }
                if worker_session.backend != BackendState::Prepared
                    || session.phase() != StreamPhase::Prepared
                {
                    worker_session.start_pending = false;
                    let error = if session.phase() == StreamPhase::Xrun {
                        SoundError::Xrun
                    } else {
                        SoundError::BadState
                    };
                    self.complete_user_opt(control, Err(error));
                    return SubmitOutcome::Immediate;
                }
                let Some(stream) = worker_session.stream else {
                    self.complete_user_opt(control, Err(SoundError::BadState));
                    return SubmitOutcome::Immediate;
                };
                worker_session.start_pending = true;
                match self.card.start(stream, now, deadline) {
                    Ok(token) => SubmitOutcome::Submitted(token),
                    Err(PcmError::Again) => SubmitOutcome::Retry,
                    Err(error) => {
                        worker_session.start_pending = false;
                        self.warn_fatal_pcm(
                            "start_submit",
                            error,
                            FatalPcmContext {
                                session: Some(worker_session.id),
                                direction: Some(worker_session.direction),
                                stream: Some(stream.stream_id()),
                                ..FatalPcmContext::default()
                            },
                        );
                        let error = SoundError::from(error);
                        self.complete_user_opt(control, Err(error));
                        if error.is_fatal() {
                            self.fail_device(error);
                        }
                        SubmitOutcome::Immediate
                    }
                }
            }
            WorkerAction::Drain => {
                let Some(session) = session else {
                    self.complete_user_opt(control, Err(SoundError::NoDevice));
                    return SubmitOutcome::Immediate;
                };
                self.begin_drain(control, worker_session, session)
            }
            WorkerAction::StopForLifecycle => {
                let Some(stream) = worker_session.stream else {
                    return SubmitOutcome::Immediate;
                };
                match self.card.stop(stream, now, deadline) {
                    Ok(token) => SubmitOutcome::Submitted(token),
                    Err(PcmError::Again) => SubmitOutcome::Retry,
                    Err(error) => {
                        self.warn_fatal_pcm(
                            "stop_lifecycle_submit",
                            error,
                            FatalPcmContext {
                                session: Some(worker_session.id),
                                direction: Some(worker_session.direction),
                                stream: Some(stream.stream_id()),
                                ..FatalPcmContext::default()
                            },
                        );
                        self.lifecycle_card_error(worker_session, SoundError::from(error), false);
                        SubmitOutcome::Immediate
                    }
                }
            }
            WorkerAction::ReleaseForLifecycle => {
                let Some(stream) = worker_session.stream else {
                    return SubmitOutcome::Immediate;
                };
                match self.card.release(stream, now, deadline) {
                    Ok(token) => SubmitOutcome::Submitted(token),
                    Err(PcmError::Again) => SubmitOutcome::Retry,
                    Err(error) => {
                        self.warn_fatal_pcm(
                            "release_lifecycle_submit",
                            error,
                            FatalPcmContext {
                                session: Some(worker_session.id),
                                direction: Some(worker_session.direction),
                                stream: Some(stream.stream_id()),
                                ..FatalPcmContext::default()
                            },
                        );
                        self.lifecycle_card_error(worker_session, SoundError::from(error), false);
                        SubmitOutcome::Immediate
                    }
                }
            }
            WorkerAction::ConfigureForLifecycle => {
                let Some(stream) = worker_session.stream else {
                    self.lifecycle_card_error(worker_session, SoundError::BadState, true);
                    return SubmitOutcome::Immediate;
                };
                let Some(config) = self.lifecycle_config(worker_session) else {
                    self.lifecycle_card_error(worker_session, SoundError::BadState, true);
                    return SubmitOutcome::Immediate;
                };
                match self.card.configure(stream, config, now, deadline) {
                    Ok(token) => SubmitOutcome::Submitted(token),
                    Err(PcmError::Again) => SubmitOutcome::Retry,
                    Err(error) => {
                        self.warn_fatal_pcm(
                            "configure_lifecycle_submit",
                            error,
                            FatalPcmContext {
                                session: Some(worker_session.id),
                                direction: Some(worker_session.direction),
                                stream: Some(stream.stream_id()),
                                ..FatalPcmContext::default()
                            },
                        );
                        self.lifecycle_card_error(worker_session, SoundError::from(error), true);
                        SubmitOutcome::Immediate
                    }
                }
            }
            WorkerAction::PrepareForLifecycle => {
                let Some(stream) = worker_session.stream else {
                    self.lifecycle_card_error(worker_session, SoundError::BadState, true);
                    return SubmitOutcome::Immediate;
                };
                match self.card.prepare(stream, now, deadline) {
                    Ok(token) => SubmitOutcome::Submitted(token),
                    Err(PcmError::Again) => SubmitOutcome::Retry,
                    Err(error) => {
                        self.warn_fatal_pcm(
                            "prepare_lifecycle_submit",
                            error,
                            FatalPcmContext {
                                session: Some(worker_session.id),
                                direction: Some(worker_session.direction),
                                stream: Some(stream.stream_id()),
                                ..FatalPcmContext::default()
                            },
                        );
                        self.lifecycle_card_error(worker_session, SoundError::from(error), true);
                        SubmitOutcome::Immediate
                    }
                }
            }
            WorkerAction::StopForDrain => {
                let Some(stream) = worker_session.stream else {
                    self.complete_user_opt(control, Err(SoundError::BadState));
                    return SubmitOutcome::Immediate;
                };
                match self.card.stop(stream, now, deadline) {
                    Ok(token) => SubmitOutcome::Submitted(token),
                    Err(PcmError::Again) => SubmitOutcome::Retry,
                    Err(error) => {
                        self.warn_fatal_pcm(
                            "stop_drain_submit",
                            error,
                            FatalPcmContext {
                                session: Some(worker_session.id),
                                direction: Some(worker_session.direction),
                                stream: Some(stream.stream_id()),
                                ..FatalPcmContext::default()
                            },
                        );
                        let error = SoundError::from(error);
                        self.complete_user_opt(control, Err(error));
                        self.fail_device(error);
                        SubmitOutcome::Immediate
                    }
                }
            }
        }
    }

    /// Publishes Linux drain-init state, then completes the control request.
    /// Nonblocking callers return EAGAIN at that point; playback completion and
    /// DMA shutdown continue through the worker pump.
    fn begin_drain(
        &mut self,
        control: &mut ReadyControl,
        worker_session: &mut WorkerSession,
        session: &Arc<PcmSession>,
    ) -> SubmitOutcome {
        let phase = session.phase();
        if phase == StreamPhase::Open {
            self.complete_user_opt(control, Err(SoundError::BadState));
            return SubmitOutcome::Immediate;
        }
        if phase == StreamPhase::Failed {
            self.complete_user_opt(control, Err(SoundError::NoDevice));
            return SubmitOutcome::Immediate;
        }
        if session.direction() == PcmDirection::Playback {
            match phase {
                StreamPhase::Xrun => {
                    session.mark_setup();
                    if worker_session.backend == BackendState::Running {
                        return self.submit_stop_for_drain(control, worker_session);
                    }
                }
                StreamPhase::Prepared => {
                    if !session.playback_is_idle() {
                        control.action = WorkerAction::StartForDrain;
                        return SubmitOutcome::Retry;
                    }
                    session.mark_setup();
                }
                StreamPhase::Running => {
                    if !session.playback_is_idle() {
                        session.mark_draining();
                    } else if worker_session.backend == BackendState::Running {
                        session.mark_draining();
                        return self.submit_stop_for_drain(control, worker_session);
                    } else {
                        session.mark_setup();
                    }
                }
                StreamPhase::Setup | StreamPhase::Draining => {}
                StreamPhase::Open | StreamPhase::Failed => unreachable!("checked above"),
            }
            self.complete_user_opt(control, Ok(()));
            SubmitOutcome::Immediate
        } else {
            if phase == StreamPhase::Running {
                if session.capture_available().unwrap_or(0) > 0 {
                    session.mark_draining();
                } else {
                    session.mark_setup();
                }
                if worker_session.backend == BackendState::Running {
                    return self.submit_stop_for_drain(control, worker_session);
                }
            }
            self.complete_user_opt(control, Ok(()));
            SubmitOutcome::Immediate
        }
    }

    fn submit_stop_for_drain(
        &mut self,
        control: &mut ReadyControl,
        worker_session: &mut WorkerSession,
    ) -> SubmitOutcome {
        worker_session.drain_stop_queued = true;
        control.action = WorkerAction::StopForDrain;
        let Some(stream) = worker_session.stream else {
            self.complete_user_opt(control, Err(SoundError::BadState));
            return SubmitOutcome::Immediate;
        };
        let now = now_ns();
        match self
            .card
            .stop(stream, now, now.saturating_add(CONTROL_TIMEOUT_NS))
        {
            Ok(token) => SubmitOutcome::Submitted(token),
            Err(PcmError::Again) => SubmitOutcome::Retry,
            Err(error) => {
                self.warn_fatal_pcm(
                    "stop_drain_initial_submit",
                    error,
                    FatalPcmContext {
                        session: Some(worker_session.id),
                        direction: Some(worker_session.direction),
                        stream: Some(stream.stream_id()),
                        ..FatalPcmContext::default()
                    },
                );
                let error = SoundError::from(error);
                self.complete_user_opt(control, Err(error));
                self.fail_device(error);
                SubmitOutcome::Immediate
            }
        }
    }

    fn queue_drain_after_start(&mut self, control: &mut ReadyControl) {
        self.controls.push_front(ReadyControl {
            session_id: control.session_id,
            action: WorkerAction::Drain,
            user: control.user.take(),
        });
    }

    fn complete_user_opt(&self, control: &mut ReadyControl, result: Result<(), SoundError>) {
        if let Some(request) = control.user.take() {
            self.complete_user(&request, result);
        }
    }
}
