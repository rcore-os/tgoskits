//! PCM session state and per-generation policy.

use rdif_audio::{
    PcmBuffer, PcmCapabilities, PcmCard, PcmCardState, PcmCompletion, PcmConfig, PcmControlOp,
    PcmControlToken, PcmDirection, PcmError, PcmIoToken, PcmServiceBudget, PcmServiceReport,
    PcmStream,
};
use virtio_drivers::{Hal, transport::Transport};

use super::{
    VirtioSound,
    buffers::{DirectionBuffers, find_rx_recycle_slot},
    control::ControlKind,
};
use crate::wire::{
    FORMAT_S16, R_PCM_PREPARE, R_PCM_RELEASE, R_PCM_START, R_PCM_STOP, RATE_48000,
    pcm_set_params_request,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SessionState {
    Opened,
    Configuring,
    Configured,
    Preparing,
    Prepared,
    Starting,
    Running,
    Stopping,
    Stopped,
    Releasing,
    Released,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum DeferredRelease {
    Waiting(PcmControlToken),
    Terminal {
        token: PcmControlToken,
        result: Result<(), PcmError>,
    },
}

pub(super) const fn release_slot(direction: PcmDirection) -> usize {
    match direction {
        PcmDirection::Playback => 0,
        PcmDirection::Capture => 1,
    }
}

pub(super) struct StreamSession {
    pub(super) stream: PcmStream,
    pub(super) config: Option<PcmConfig>,
    pub(super) state: SessionState,
    pub(super) release_acked: bool,
    pub(super) release_deadline_ns: Option<u64>,
    pub(super) buffers: DirectionBuffers,
}

impl StreamSession {
    pub(super) fn new(stream: PcmStream) -> Self {
        Self {
            stream,
            config: None,
            state: SessionState::Opened,
            release_acked: false,
            release_deadline_ns: None,
            buffers: DirectionBuffers::empty(),
        }
    }
}

impl<H: Hal + 'static, T: Transport + Send + 'static> PcmCard for VirtioSound<H, T> {
    fn state(&self) -> PcmCardState {
        self.state
    }

    fn capabilities(&self) -> Option<&PcmCapabilities> {
        self.discovered.then_some(&self.capabilities)
    }

    fn open(&mut self, direction: PcmDirection) -> Result<PcmStream, PcmError> {
        if self.state != PcmCardState::Ready {
            return Err(PcmError::NotReady);
        }
        let capabilities = self.supports(direction).ok_or(PcmError::Unsupported)?;
        let occupied = match direction {
            PcmDirection::Playback => self.playback.is_some(),
            PcmDirection::Capture => self.capture.is_some(),
        };
        if occupied {
            return Err(PcmError::Busy);
        }
        let generation = self.next_generation;
        self.next_generation = generation.checked_add(1).ok_or(PcmError::TokenExhausted)?;
        let stream = PcmStream::new(capabilities.stream_id, direction, generation);
        let session = StreamSession::new(stream);
        match direction {
            PcmDirection::Playback => self.playback = Some(session),
            PcmDirection::Capture => self.capture = Some(session),
        }
        Ok(stream)
    }

    fn configure(
        &mut self,
        stream: PcmStream,
        config: PcmConfig,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError> {
        if self.state != PcmCardState::Ready {
            return Err(PcmError::NotReady);
        }
        if deadline_ns == 0 || deadline_ns <= now_ns {
            return Err(PcmError::DeadlineExpired);
        }
        if self.control_pending.is_some() {
            return Err(PcmError::Busy);
        }
        if !self.supported_config(stream.direction(), config) {
            return Err(PcmError::Unsupported);
        }
        let period_bytes = u32::try_from(config.period_bytes().ok_or(PcmError::InvalidConfig)?)
            .map_err(|_| PcmError::InvalidConfig)?;
        let buffer_bytes = u32::try_from(config.buffer_bytes().ok_or(PcmError::InvalidConfig)?)
            .map_err(|_| PcmError::InvalidConfig)?;
        let channels = u8::try_from(config.channels).map_err(|_| PcmError::InvalidConfig)?;
        {
            let session = self.session_mut(stream)?;
            if session.state != SessionState::Opened {
                return Err(PcmError::BadState);
            }
        }
        let buffers =
            DirectionBuffers::allocate(&self.dma, config, stream.stream_id(), stream.direction())
                .map_err(|error| error.as_pcm())?;
        let token = self.allocate_control_token(stream, PcmControlOp::Configure)?;
        {
            let session = self.session_mut(stream)?;
            session.buffers = buffers;
            session.state = SessionState::Configuring;
        }
        let request = pcm_set_params_request(
            stream.stream_id(),
            buffer_bytes,
            period_bytes,
            0,
            channels,
            FORMAT_S16,
            RATE_48000,
        );
        if let Err(error) = self.submit_control(
            ControlKind::Configure(config),
            Some(token),
            deadline_ns,
            &request,
        ) {
            if let Ok(session) = self.session_mut(stream) {
                session.buffers = DirectionBuffers::empty();
                session.state = SessionState::Opened;
            }
            return Err(error);
        }
        Ok(token)
    }

    fn prepare(
        &mut self,
        stream: PcmStream,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError> {
        if self.session(stream).ok_or(PcmError::BadState)?.state != SessionState::Configured {
            return Err(PcmError::BadState);
        }
        self.submit_stream_control(
            stream,
            ControlKind::Prepare,
            R_PCM_PREPARE,
            SessionState::Preparing,
            now_ns,
            deadline_ns,
        )
    }

    fn start(
        &mut self,
        stream: PcmStream,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError> {
        if self.session(stream).ok_or(PcmError::BadState)?.state != SessionState::Prepared {
            return Err(PcmError::BadState);
        }
        self.submit_stream_control(
            stream,
            ControlKind::Start,
            R_PCM_START,
            SessionState::Starting,
            now_ns,
            deadline_ns,
        )
    }

    fn stop(
        &mut self,
        stream: PcmStream,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError> {
        if self.session(stream).ok_or(PcmError::BadState)?.state != SessionState::Running {
            return Err(PcmError::BadState);
        }
        self.submit_stream_control(
            stream,
            ControlKind::Stop,
            R_PCM_STOP,
            SessionState::Stopping,
            now_ns,
            deadline_ns,
        )
    }

    fn release(
        &mut self,
        stream: PcmStream,
        now_ns: u64,
        deadline_ns: u64,
    ) -> Result<PcmControlToken, PcmError> {
        let state = self.session(stream).ok_or(PcmError::BadState)?.state;
        if !matches!(
            state,
            SessionState::Opened
                | SessionState::Configured
                | SessionState::Prepared
                | SessionState::Stopped
        ) {
            return Err(PcmError::BadState);
        }
        let token = self.submit_stream_control(
            stream,
            ControlKind::Release,
            R_PCM_RELEASE,
            SessionState::Releasing,
            now_ns,
            deadline_ns,
        )?;
        if let Ok(session) = self.session_mut(stream) {
            session.release_deadline_ns = Some(deadline_ns);
        }
        Ok(token)
    }

    fn write_playback(
        &mut self,
        stream: PcmStream,
        pcm: &[u8],
        deadline_ns: u64,
    ) -> Result<PcmIoToken, PcmError> {
        if self.state != PcmCardState::Ready {
            return Err(PcmError::NotReady);
        }
        if deadline_ns == 0 {
            return Err(PcmError::DeadlineExpired);
        }
        self.submit_playback(stream, pcm, deadline_ns)
    }

    fn recycle_capture(
        &mut self,
        stream: PcmStream,
        buffer: PcmBuffer,
        deadline_ns: u64,
    ) -> Result<(), PcmError> {
        if buffer.owner().raw() != self.id || buffer.direction() != PcmDirection::Capture {
            return Err(PcmError::InvalidBuffer);
        }
        if self.state != PcmCardState::Ready {
            return Ok(());
        }
        if self.session(stream).is_none() {
            return Ok(());
        }
        let (state, period_bytes) = {
            let session = self.session_mut(stream)?;
            (
                session.state,
                session
                    .config
                    .and_then(PcmConfig::period_bytes)
                    .ok_or(PcmError::BadState)?,
            )
        };
        if buffer.len() != period_bytes {
            return Err(PcmError::InvalidBuffer);
        }
        if matches!(state, SessionState::Released) {
            return Ok(());
        }
        if !matches!(
            state,
            SessionState::Prepared | SessionState::Running | SessionState::Stopped
        ) {
            return Err(PcmError::BadState);
        }
        let data = buffer.into_dma();
        {
            let session = self.session_mut(stream)?;
            let index = find_rx_recycle_slot(&session.buffers).ok_or(PcmError::Again)?;
            session.buffers.data[index] = Some(data);
        }
        if matches!(state, SessionState::Prepared | SessionState::Running)
            && self.has_completion_capacity()
        {
            let deadline = if matches!(state, SessionState::Running) {
                deadline_ns
            } else {
                0
            };
            match self.prearm_capture(stream, 1, deadline) {
                Ok(()) | Err(PcmError::Again) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn take_completion(&mut self) -> Option<PcmCompletion> {
        if self.completion_len == 0 {
            return None;
        }
        let completion = self.completions[self.completion_head].take();
        self.completion_head = (self.completion_head + 1) % super::COMPLETION_CAPACITY;
        self.completion_len -= 1;
        completion
    }

    fn service(&mut self, now_ns: u64, budget: PcmServiceBudget) -> PcmServiceReport {
        self.service_queues(now_ns, budget)
    }

    fn begin_shutdown(&mut self, now_ns: u64, deadline_ns: u64) -> Result<(), PcmError> {
        if self.state == PcmCardState::Stopped {
            return Ok(());
        }
        if deadline_ns == 0 || deadline_ns <= now_ns {
            return Err(PcmError::DeadlineExpired);
        }
        self.fail_waiting_releases(PcmError::ResetPending);
        if !matches!(
            self.state,
            PcmCardState::Resetting | PcmCardState::ShuttingDown
        ) {
            self.state = PcmCardState::ShuttingDown;
            self.state_changed = true;
        }
        self.shutdown_deadline_ns = Some(deadline_ns);
        self.transport
            .set_status(virtio_drivers::transport::DeviceStatus::empty());
        Ok(())
    }
}
