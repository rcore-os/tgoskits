use alloc::sync::Arc;

use rdif_audio::PcmError;

use super::{BackendState, LifecycleGoal, ReadyControl, Worker, WorkerAction, WorkerSession};
use crate::sound::{SoundError, protocol::SessionId, session::PcmSession};

impl Worker {
    pub(super) fn lifecycle_config(
        &self,
        worker_session: &WorkerSession,
    ) -> Option<rdif_audio::PcmConfig> {
        match worker_session.lifecycle.as_ref()?.goal {
            LifecycleGoal::Configure { config, .. } => Some(config),
            _ => worker_session.config,
        }
    }

    pub(super) fn lifecycle_card_error(
        &mut self,
        worker_session: &mut WorkerSession,
        error: SoundError,
        cleanup: bool,
    ) {
        let close = worker_session
            .lifecycle
            .as_ref()
            .is_some_and(|lifecycle| matches!(lifecycle.goal, LifecycleGoal::Close));
        if close || !cleanup || error.is_fatal() {
            if let Some(lifecycle) = worker_session.lifecycle.take()
                && let Some(request) = lifecycle.user
            {
                self.complete_user(&request, Err(error));
            }
            self.fail_device(error);
            return;
        }
        if let Some(lifecycle) = worker_session.lifecycle.as_mut() {
            lifecycle.goal = LifecycleGoal::CleanupOpen;
            lifecycle.error = Some(error);
        }
        self.service.notify_worker();
    }

    pub(super) fn drive_lifecycle(
        &mut self,
        worker_session: &mut WorkerSession,
        session: Option<&Arc<PcmSession>>,
    ) {
        if self
            .current
            .as_ref()
            .is_some_and(|current| current.control.session_id == worker_session.id)
            || self
                .controls
                .iter()
                .any(|control| control.session_id == worker_session.id)
        {
            return;
        }
        let Some(lifecycle) = worker_session.lifecycle.as_ref() else {
            return;
        };
        let goal = lifecycle.goal;
        let need_reset = lifecycle.need_reset;
        let reset_done = lifecycle.reset_done;
        if self.failed && !matches!(goal, LifecycleGoal::Close) {
            if let Some(lifecycle) = worker_session.lifecycle.take()
                && let Some(request) = lifecycle.user
            {
                self.complete_user(&request, Err(SoundError::NoDevice));
            }
            return;
        }

        match goal {
            LifecycleGoal::Close => {
                if worker_session.backend == BackendState::Running {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::StopForLifecycle);
                } else if worker_session.stream.is_some() {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::ReleaseForLifecycle);
                } else {
                    self.service.release_slot(worker_session.id);
                    worker_session.lifecycle = None;
                }
            }
            LifecycleGoal::HwFree => {
                if worker_session.backend == BackendState::Running {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::StopForLifecycle);
                } else if worker_session.stream.is_some() {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::ReleaseForLifecycle);
                } else {
                    self.finish_lifecycle(worker_session, session);
                }
            }
            LifecycleGoal::CleanupOpen => {
                if worker_session.stream.is_some() {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::ReleaseForLifecycle);
                } else {
                    self.finish_lifecycle(worker_session, session);
                }
            }
            LifecycleGoal::Prepare => match worker_session.backend {
                BackendState::Running => {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::StopForLifecycle);
                }
                BackendState::Released => self.open_lifecycle_stream(worker_session),
                BackendState::Opened => {
                    self.push_lifecycle_step(
                        worker_session.id,
                        WorkerAction::ConfigureForLifecycle,
                    );
                }
                BackendState::Configured => {
                    if need_reset && !reset_done {
                        self.push_lifecycle_step(
                            worker_session.id,
                            WorkerAction::ReleaseForLifecycle,
                        );
                    } else {
                        self.push_lifecycle_step(
                            worker_session.id,
                            WorkerAction::PrepareForLifecycle,
                        );
                    }
                }
                BackendState::Prepared => {
                    if need_reset && !reset_done {
                        self.push_lifecycle_step(
                            worker_session.id,
                            WorkerAction::ReleaseForLifecycle,
                        );
                    } else {
                        self.finish_lifecycle(worker_session, session);
                    }
                }
                BackendState::Stopped => {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::ReleaseForLifecycle);
                }
            },
            LifecycleGoal::Drop => match worker_session.backend {
                BackendState::Running => {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::StopForLifecycle);
                }
                BackendState::Released => self.open_lifecycle_stream(worker_session),
                BackendState::Opened => {
                    self.push_lifecycle_step(
                        worker_session.id,
                        WorkerAction::ConfigureForLifecycle,
                    );
                }
                BackendState::Configured => {
                    if need_reset && !reset_done {
                        self.push_lifecycle_step(
                            worker_session.id,
                            WorkerAction::ReleaseForLifecycle,
                        );
                    } else {
                        self.finish_lifecycle(worker_session, session);
                    }
                }
                BackendState::Prepared | BackendState::Stopped => {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::ReleaseForLifecycle);
                }
            },
            LifecycleGoal::Configure { .. } => match worker_session.backend {
                BackendState::Running => {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::StopForLifecycle);
                }
                BackendState::Released => self.open_lifecycle_stream(worker_session),
                BackendState::Opened => {
                    self.push_lifecycle_step(
                        worker_session.id,
                        WorkerAction::ConfigureForLifecycle,
                    );
                }
                BackendState::Configured => {
                    if need_reset && !reset_done {
                        self.push_lifecycle_step(
                            worker_session.id,
                            WorkerAction::ReleaseForLifecycle,
                        );
                    } else {
                        self.finish_lifecycle(worker_session, session);
                    }
                }
                BackendState::Prepared | BackendState::Stopped => {
                    self.push_lifecycle_step(worker_session.id, WorkerAction::ReleaseForLifecycle);
                }
            },
        }
    }

    fn push_lifecycle_step(&mut self, session_id: SessionId, action: WorkerAction) {
        self.controls.push_back(ReadyControl {
            session_id,
            action,
            user: None,
        });
    }

    fn open_lifecycle_stream(&mut self, worker_session: &mut WorkerSession) {
        match self.card.open(worker_session.direction) {
            Ok(stream) => {
                worker_session.stream = Some(stream);
                worker_session.backend = BackendState::Opened;
                worker_session.playback = None;
                worker_session.capture_hold.clear();
            }
            Err(PcmError::Again) => {}
            Err(error) => {
                self.warn_fatal_pcm(
                    "lifecycle_stream_open",
                    error,
                    super::FatalPcmContext {
                        session: Some(worker_session.id),
                        direction: Some(worker_session.direction),
                        ..super::FatalPcmContext::default()
                    },
                );
                self.lifecycle_card_error(worker_session, SoundError::from(error), true);
            }
        }
    }

    fn finish_lifecycle(
        &mut self,
        worker_session: &mut WorkerSession,
        session: Option<&Arc<PcmSession>>,
    ) {
        let Some(lifecycle) = worker_session.lifecycle.take() else {
            return;
        };
        let result: Result<(), SoundError> = lifecycle.error.map_or(Ok(()), Err);
        match lifecycle.goal {
            LifecycleGoal::Prepare => {
                if let Some(session) = session {
                    session.mark_prepared();
                }
            }
            LifecycleGoal::Drop => {
                if let Some(session) = session {
                    session.mark_setup();
                }
            }
            LifecycleGoal::HwFree => {
                if let Some(session) = session {
                    session.mark_open();
                }
            }
            LifecycleGoal::Configure { format, config } => {
                if let Some(session) = session {
                    session.mark_configured(format, config);
                }
            }
            LifecycleGoal::CleanupOpen => {
                if let Some(session) = session {
                    session.mark_open();
                }
            }
            LifecycleGoal::Close => {
                self.service.release_slot(worker_session.id);
                return;
            }
        }
        if let Some(request) = lifecycle.user {
            self.complete_user(&request, result);
        }
    }
}
