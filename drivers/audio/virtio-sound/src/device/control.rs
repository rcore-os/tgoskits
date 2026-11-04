//! VirtIO Sound control transactions and discovery.

use core::{mem::size_of, slice};

use dma_api::InFlightDma;
use rdif_audio::{
    MAX_PERIOD_COUNT, MAX_PERIOD_FRAMES, MIN_PERIOD_COUNT, MIN_PERIOD_FRAMES, PcmCapabilities,
    PcmCardState, PcmCompletion, PcmConfig, PcmControlOp, PcmControlToken, PcmDirection, PcmError,
    PcmStreamCapabilities, SampleFormat,
};
use virtio_drivers::{Hal, transport::Transport};

use super::{
    VirtioSound,
    buffers::{DirectionBuffers, MAX_PERIODS},
    session::{DeferredRelease, SessionState, release_slot},
};
use crate::{
    error::virtio_pcm_error,
    wire::{
        CONTROL_QUEUE, CONTROL_REQUEST_SIZE, DIRECTION_INPUT, DIRECTION_OUTPUT, FORMAT_S16_BIT,
        PCM_INFO_SIZE, RATE_48000_BIT, ResponseStatus, parse_pcm_info, pcm_header_request,
        response_status,
    },
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ControlKind {
    Discovery { stream_count: u32 },
    Configure(PcmConfig),
    Prepare,
    Start,
    Stop,
    Release,
}

pub(super) struct ControlInFlight {
    pub(super) queue_token: u16,
    pub(super) token: Option<PcmControlToken>,
    pub(super) kind: ControlKind,
    pub(super) request_len: usize,
    pub(super) deadline_ns: u64,
    pub(super) timeout_reported: bool,
    pub(super) request: InFlightDma,
    pub(super) response: InFlightDma,
}

impl<H: Hal, T: Transport> VirtioSound<H, T> {
    pub(super) fn submit_control(
        &mut self,
        kind: ControlKind,
        token: Option<PcmControlToken>,
        deadline_ns: u64,
        request_bytes: &[u8],
    ) -> Result<(), PcmError> {
        if self.queues_dropped || self.control_pending.is_some() || !self.has_completion_capacity()
        {
            return Err(PcmError::Busy);
        }
        if deadline_ns == 0 || request_bytes.len() > CONTROL_REQUEST_SIZE {
            return Err(PcmError::DeadlineExpired);
        }
        let mut request_cpu = self.control_request_pool.take().ok_or(PcmError::Busy)?;
        let response_cpu = match self.control_response_pool.take() {
            Some(response) => response,
            None => {
                self.control_request_pool = Some(request_cpu);
                return Err(PcmError::Busy);
            }
        };
        request_cpu.copy_from_slice_cpu(request_bytes);

        let request = request_cpu.prepare_for_device();
        let response = response_cpu.prepare_for_device();
        // SAFETY: the DMA owners are moved into the control pending entry only
        // after `add` succeeds. The temporary slices end immediately after the
        // call; the device gets separate physical addresses and no Rust
        // reference survives.
        let (add_result, should_notify) = unsafe {
            let Some(queue_owner) = self.control.as_mut() else {
                return Err(PcmError::BadState);
            };
            let request_slice =
                slice::from_raw_parts(request.cpu_ptr().as_ptr(), request_bytes.len());
            let response_slice =
                slice::from_raw_parts_mut(response.cpu_ptr().as_ptr(), response.len().get());
            (
                queue_owner.add(&[request_slice], &mut [response_slice]),
                queue_owner.should_notify(),
            )
        };
        let queue_token = match add_result {
            Ok(token) => token,
            Err(error) => {
                let request_cpu = request.complete_without_device().into_cpu_buffer();
                let response_cpu = response.complete_without_device().into_cpu_buffer();
                self.control_request_pool = Some(request_cpu);
                self.control_response_pool = Some(response_cpu);
                return Err(virtio_pcm_error(error));
            }
        };
        if should_notify {
            self.transport.notify(CONTROL_QUEUE);
        }
        // SAFETY: the descriptors are published and both owners stay in the
        // pending entry until the matching used-ring entry is observed.
        let (request, response) = unsafe { (request.into_in_flight(), response.into_in_flight()) };
        self.control_pending = Some(ControlInFlight {
            queue_token,
            token,
            kind,
            request_len: request_bytes.len(),
            deadline_ns,
            timeout_reported: false,
            request,
            response,
        });
        Ok(())
    }

    fn control_timeout(&mut self, now_ns: u64) -> bool {
        let token = match self.control_pending.as_mut() {
            Some(control) if !control.timeout_reported && now_ns > control.deadline_ns => {
                control.timeout_reported = true;
                control.token
            }
            _ => return false,
        };
        if let Some(token) = token {
            let _ = self.push_completion(PcmCompletion::Control {
                token,
                result: Err(PcmError::Timeout),
            });
        }
        self.request_reset(PcmError::Timeout);
        true
    }

    pub(super) fn service_control(&mut self, now_ns: u64) -> Result<bool, PcmError> {
        if self.control_pending.is_none() || self.completion_capacity() < 2 {
            return Ok(false);
        }
        let expected_token = match self.control_pending.as_ref() {
            Some(control) => control.queue_token,
            None => return Ok(false),
        };
        let used_token = {
            let Some(queue_owner) = self.control.as_mut() else {
                return Err(PcmError::BadState);
            };
            queue_owner.peek_used()
        };
        let Some(used_token) = used_token else {
            return Ok(self.control_timeout(now_ns));
        };
        if used_token != expected_token {
            self.request_reset(PcmError::Protocol);
            return Err(PcmError::Protocol);
        }

        let control = self.control_pending.take().ok_or(PcmError::Protocol)?;
        // SAFETY: the device exposed this exact used-ring token. Both DMA
        // owners remain at their original addresses in `control` until pop
        // returns and the device is done with this descriptor chain.
        let used_len = unsafe {
            let request =
                slice::from_raw_parts(control.request.cpu_ptr().as_ptr(), control.request_len);
            let response = slice::from_raw_parts_mut(
                control.response.cpu_ptr().as_ptr(),
                control.response.len().get(),
            );
            let Some(queue_owner) = self.control.as_mut() else {
                return Err(PcmError::BadState);
            };
            queue_owner.pop_used(control.queue_token, &[request], &mut [response])
        };
        let used_len = match used_len {
            Ok(len) => len,
            Err(error) => {
                self.control_pending = Some(control);
                self.request_reset(virtio_pcm_error(error));
                return Err(virtio_pcm_error(error));
            }
        };
        // SAFETY: pop_used observed the matching used entry and unshared both
        // buffers, so hardware no longer accesses them.
        let (request, response) = unsafe {
            (
                control.request.complete_after_quiesce(),
                control.response.complete_after_quiesce(),
            )
        };
        self.control_request_pool = Some(request.into_cpu_buffer());
        let response = response.into_cpu_buffer();
        let response_len = response.as_slice_cpu().len();
        let used_len = match usize::try_from(used_len) {
            Ok(len) if (4..=response_len).contains(&len) => len,
            _ => {
                self.control_response_pool = Some(response);
                self.request_reset(PcmError::Protocol);
                return Err(PcmError::Protocol);
            }
        };
        let bytes = &response.as_slice_cpu()[..used_len];
        let Some(status) = response_status(bytes) else {
            self.control_response_pool = Some(response);
            self.request_reset(PcmError::Protocol);
            return Err(PcmError::Protocol);
        };

        let result = match control.kind {
            ControlKind::Discovery { stream_count } => {
                self.finish_discovery(status, bytes, stream_count)
            }
            _ => {
                if used_len != size_of::<u32>() {
                    self.control_response_pool = Some(response);
                    self.request_reset(PcmError::Protocol);
                    return Err(PcmError::Protocol);
                }
                map_status(status)
            }
        };
        self.control_response_pool = Some(response);

        if let Some(token) = control.token {
            let defer_success = matches!(control.kind, ControlKind::Release);
            let completion_result = match result {
                Ok(()) => {
                    if let Err(error) = self.apply_control_success(control.kind, token) {
                        self.request_reset(error);
                        Some(Err(error))
                    } else if defer_success {
                        None
                    } else {
                        Some(Ok(()))
                    }
                }
                Err(error) => {
                    self.apply_control_failure(control.kind, token);
                    if error == PcmError::Device || error == PcmError::Protocol {
                        self.request_reset(error);
                    }
                    Some(Err(error))
                }
            };
            if let Some(result) = completion_result {
                let _ = self.push_completion(PcmCompletion::Control { token, result });
            }
        } else if let Err(error) = result {
            self.request_reset(error);
        }
        Ok(true)
    }

    fn finish_discovery(
        &mut self,
        status: ResponseStatus,
        bytes: &[u8],
        stream_count: u32,
    ) -> Result<(), PcmError> {
        map_status(status)?;
        let expected = (stream_count as usize)
            .checked_mul(PCM_INFO_SIZE)
            .and_then(|bytes| bytes.checked_add(4))
            .ok_or(PcmError::Protocol)?;
        if bytes.len() != expected {
            return Err(PcmError::Protocol);
        }
        let mut capabilities = PcmCapabilities::default();
        for index in 0..stream_count as usize {
            let offset = index
                .checked_mul(PCM_INFO_SIZE)
                .and_then(|bytes| bytes.checked_add(4))
                .ok_or(PcmError::Protocol)?;
            let end = offset
                .checked_add(PCM_INFO_SIZE)
                .ok_or(PcmError::Protocol)?;
            let Some(info) = parse_pcm_info(&bytes[offset..end]) else {
                return Err(PcmError::Protocol);
            };
            if info.formats & FORMAT_S16_BIT == 0
                || info.rates & RATE_48000_BIT == 0
                || info.channels_min > 2
                || info.channels_max < 2
            {
                continue;
            }
            let direction = match info.direction {
                DIRECTION_OUTPUT => PcmDirection::Playback,
                DIRECTION_INPUT => PcmDirection::Capture,
                _ => continue,
            };
            let stream = PcmStreamCapabilities {
                stream_id: index as u32,
                direction,
                sample_format: SampleFormat::S16Le,
                sample_rate_hz: 48_000,
                channels_min: 2,
                channels_max: 2,
                period_frames_min: MIN_PERIOD_FRAMES,
                period_frames_max: MAX_PERIOD_FRAMES,
                period_count_min: MIN_PERIOD_COUNT,
                period_count_max: MAX_PERIOD_COUNT,
            };
            match direction {
                PcmDirection::Playback if capabilities.playback.is_none() => {
                    capabilities.playback = Some(stream);
                }
                PcmDirection::Capture if capabilities.capture.is_none() => {
                    capabilities.capture = Some(stream);
                }
                _ => {}
            }
        }
        self.capabilities = capabilities;
        self.discovered = true;
        self.state = PcmCardState::Ready;
        self.state_changed = true;
        let _ = self.push_completion(PcmCompletion::Capabilities);
        Ok(())
    }

    fn apply_control_success(
        &mut self,
        kind: ControlKind,
        token: PcmControlToken,
    ) -> Result<(), PcmError> {
        let stream = token.stream();
        match kind {
            ControlKind::Discovery { .. } => Err(PcmError::Protocol),
            ControlKind::Configure(config) => {
                let session = self.session_mut(stream)?;
                if session.state != SessionState::Configuring {
                    return Err(PcmError::Protocol);
                }
                session.config = Some(config);
                session.state = SessionState::Configured;
                Ok(())
            }
            ControlKind::Prepare => {
                let session = self.session_mut(stream)?;
                if session.state != SessionState::Preparing {
                    return Err(PcmError::Protocol);
                }
                session.state = SessionState::Prepared;
                if stream.direction() == PcmDirection::Capture {
                    self.prearm_capture(stream, MAX_PERIODS, 0)?;
                }
                Ok(())
            }
            ControlKind::Start => {
                let session = self.session_mut(stream)?;
                if session.state != SessionState::Starting {
                    return Err(PcmError::Protocol);
                }
                session.state = SessionState::Running;
                Ok(())
            }
            ControlKind::Stop => {
                let session = self.session_mut(stream)?;
                if session.state != SessionState::Stopping {
                    return Err(PcmError::Protocol);
                }
                session.state = SessionState::Stopped;
                Ok(())
            }
            ControlKind::Release => {
                let slot = release_slot(stream.direction());
                if self.deferred_release[slot].is_some() {
                    return Err(PcmError::Protocol);
                }
                let session = self.session_mut(stream)?;
                if session.state != SessionState::Releasing {
                    return Err(PcmError::Protocol);
                }
                session.state = SessionState::Released;
                session.release_acked = true;
                self.deferred_release[slot] = Some(DeferredRelease::Waiting(token));
                Ok(())
            }
        }
    }

    fn apply_control_failure(&mut self, kind: ControlKind, token: PcmControlToken) {
        let Ok(session) = self.session_mut(token.stream()) else {
            return;
        };
        match kind {
            ControlKind::Configure(_) => {
                session.config = None;
                session.buffers = DirectionBuffers::empty();
                session.state = SessionState::Opened;
            }
            ControlKind::Prepare => session.state = SessionState::Configured,
            ControlKind::Start => session.state = SessionState::Prepared,
            ControlKind::Stop => session.state = SessionState::Running,
            ControlKind::Release => {
                session.state = SessionState::Stopped;
                session.release_deadline_ns = None;
            }
            ControlKind::Discovery { .. } => {}
        }
    }

    pub(super) fn allocate_control_token(
        &mut self,
        stream: rdif_audio::PcmStream,
        operation: PcmControlOp,
    ) -> Result<PcmControlToken, PcmError> {
        let sequence = self.next_control_sequence;
        self.next_control_sequence = sequence.checked_add(1).ok_or(PcmError::TokenExhausted)?;
        Ok(PcmControlToken::new(stream, operation, sequence))
    }

    pub(super) fn submit_stream_control(
        &mut self,
        stream: rdif_audio::PcmStream,
        kind: ControlKind,
        code: u32,
        pending_state: SessionState,
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
        let operation = match kind {
            ControlKind::Prepare => PcmControlOp::Prepare,
            ControlKind::Start => PcmControlOp::Start,
            ControlKind::Stop => PcmControlOp::Stop,
            ControlKind::Release => PcmControlOp::Release,
            ControlKind::Discovery { .. } | ControlKind::Configure(_) => {
                return Err(PcmError::BadState);
            }
        };
        let previous_state = self.session(stream).ok_or(PcmError::BadState)?.state;
        let token = self.allocate_control_token(stream, operation)?;
        {
            let session = self.session_mut(stream)?;
            session.state = pending_state;
        }
        let request = pcm_header_request(code, stream.stream_id());
        if let Err(error) = self.submit_control(kind, Some(token), deadline_ns, &request) {
            if let Ok(session) = self.session_mut(stream) {
                session.state = previous_state;
            }
            return Err(error);
        }
        Ok(token)
    }
}

pub(super) fn map_status(status: ResponseStatus) -> Result<(), PcmError> {
    match status {
        ResponseStatus::Ok => Ok(()),
        ResponseStatus::NotSupported => Err(PcmError::Unsupported),
        ResponseStatus::BadMessage | ResponseStatus::Unknown(_) => Err(PcmError::Protocol),
        ResponseStatus::IoError => Err(PcmError::Device),
    }
}
