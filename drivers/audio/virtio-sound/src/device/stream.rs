//! Playback, capture and event queue progress.

use core::slice;

use rdif_audio::{
    PcmBuffer, PcmBufferOwner, PcmCompletion, PcmConfig, PcmDirection, PcmError, PcmEvent,
    PcmEventKind, PcmIoToken,
};

use super::{
    VirtioSound,
    buffers::{EventInFlight, RxInFlight, TxInFlight, find_complete_slot, find_rx_slot},
    session::SessionState,
};
use crate::{
    error::virtio_pcm_error,
    wire::{
        EVENT_QUEUE, EVENT_SIZE, EVT_JACK_CONNECTED, EVT_JACK_DISCONNECTED, EVT_PCM_PERIOD_ELAPSED,
        EVT_PCM_XRUN, PCM_STATUS_SIZE, RX_QUEUE, ResponseStatus, TX_QUEUE, event, pcm_status,
    },
};

impl<H: virtio_drivers::Hal, T: virtio_drivers::transport::Transport> VirtioSound<H, T> {
    pub(super) fn refill_events(&mut self, max_submissions: usize) -> Result<(), PcmError> {
        if self.queues_dropped {
            return Ok(());
        }
        let mut submitted = 0;
        for (index, inflight) in self.event_inflight.iter_mut().enumerate() {
            if submitted >= max_submissions {
                break;
            }
            if inflight.is_some() {
                continue;
            }
            let Some(buffer_cpu) = self.event_pool[index].take() else {
                continue;
            };
            let prepared = buffer_cpu.prepare_for_device();
            // SAFETY: the prepared DMA owner is moved into `event_inflight`
            // after a successful add. The output slice is temporary and the
            // device cannot access the bytes through a Rust reference.
            let (add_result, should_notify) = unsafe {
                let Some(event) = self.event.as_mut() else {
                    return Err(PcmError::BadState);
                };
                let output =
                    slice::from_raw_parts_mut(prepared.cpu_ptr().as_ptr(), prepared.len().get());
                (event.add(&[], &mut [output]), event.should_notify())
            };
            let queue_token = match add_result {
                Ok(token) => token,
                Err(error) => {
                    self.event_pool[index] =
                        Some(prepared.complete_without_device().into_cpu_buffer());
                    return Err(virtio_pcm_error(error));
                }
            };
            if should_notify {
                self.transport.notify(EVENT_QUEUE);
            }
            // SAFETY: the buffer remains owned until its exact token is popped.
            let buffer = unsafe { prepared.into_in_flight() };
            *inflight = Some(EventInFlight {
                queue_token,
                pool_index: index,
                buffer,
            });
            submitted += 1;
        }
        Ok(())
    }

    pub(super) fn submit_playback(
        &mut self,
        stream: rdif_audio::PcmStream,
        pcm: &[u8],
        deadline_ns: u64,
    ) -> Result<PcmIoToken, PcmError> {
        if pcm.is_empty() || !self.has_completion_capacity() {
            return Err(PcmError::InvalidBuffer);
        }
        let session = self.session_mut(stream)?;
        if !matches!(
            session.state,
            SessionState::Prepared | SessionState::Running
        ) {
            return Err(PcmError::BadState);
        }
        let config = session.config.ok_or(PcmError::BadState)?;
        let period_bytes = config.period_bytes().ok_or(PcmError::InvalidConfig)?;
        if pcm.len() != period_bytes
            || !pcm
                .len()
                .is_multiple_of(config.frame_bytes().ok_or(PcmError::InvalidConfig)?)
        {
            return Err(PcmError::InvalidBuffer);
        }
        let pool_index = find_complete_slot(&session.buffers).ok_or(PcmError::Again)?;
        let data_cpu = session.buffers.data[pool_index]
            .take()
            .ok_or(PcmError::Again)?;
        let header_cpu = session.buffers.headers[pool_index]
            .take()
            .ok_or(PcmError::Again)?;
        let status_cpu = session.buffers.statuses[pool_index]
            .take()
            .ok_or(PcmError::Again)?;
        let Some(inflight_index) = self.tx_inflight.iter().position(Option::is_none) else {
            self.restore_playback_buffers(stream, pool_index, data_cpu, header_cpu, status_cpu);
            return Err(PcmError::Again);
        };
        let mut data_cpu = data_cpu;
        data_cpu.copy_from_slice_cpu(pcm);
        let data = data_cpu.prepare_for_device();
        let header = header_cpu.prepare_for_device();
        let status = status_cpu.prepare_for_device();

        // SAFETY: `add` publishes only descriptor addresses. All three DMA
        // owners are moved into `tx_inflight` after success and stay there
        // until the exact used-ring token is popped.
        let (add_result, should_notify) = unsafe {
            let Some(tx) = self.tx.as_mut() else {
                return Err(PcmError::BadState);
            };
            let header_slice = slice::from_raw_parts(header.cpu_ptr().as_ptr(), header.len().get());
            let data_slice = slice::from_raw_parts(data.cpu_ptr().as_ptr(), data.len().get());
            let status_slice =
                slice::from_raw_parts_mut(status.cpu_ptr().as_ptr(), status.len().get());
            (
                tx.add(&[header_slice, data_slice], &mut [status_slice]),
                tx.should_notify(),
            )
        };
        let queue_token = match add_result {
            Ok(token) => token,
            Err(error) => {
                self.restore_playback_buffers(
                    stream,
                    pool_index,
                    data.complete_without_device().into_cpu_buffer(),
                    header.complete_without_device().into_cpu_buffer(),
                    status.complete_without_device().into_cpu_buffer(),
                );
                return Err(virtio_pcm_error(error));
            }
        };
        if should_notify {
            self.transport.notify(TX_QUEUE);
        }
        // SAFETY: the three owners remain reachable through `tx_inflight`.
        let (data, header, status) = unsafe {
            (
                data.into_in_flight(),
                header.into_in_flight(),
                status.into_in_flight(),
            )
        };
        let token = self.allocate_io_token(stream);
        self.tx_inflight[inflight_index] = Some(TxInFlight {
            queue_token,
            token,
            pool_index,
            frames: config.period_frames,
            deadline_ns,
            data,
            header,
            status,
        });
        Ok(token)
    }

    fn allocate_io_token(&mut self, stream: rdif_audio::PcmStream) -> PcmIoToken {
        let sequence = self.next_io_sequence;
        self.next_io_sequence = self.next_io_sequence.wrapping_add(1);
        if self.next_io_sequence == 0 {
            self.next_io_sequence = 1;
        }
        PcmIoToken::new(stream, sequence)
    }

    fn restore_playback_buffers(
        &mut self,
        stream: rdif_audio::PcmStream,
        index: usize,
        data: dma_api::CpuDmaBuffer,
        header: dma_api::CpuDmaBuffer,
        status: dma_api::CpuDmaBuffer,
    ) {
        if let Ok(session) = self.session_mut(stream) {
            session.buffers.data[index] = Some(data);
            session.buffers.headers[index] = Some(header);
            session.buffers.statuses[index] = Some(status);
        }
    }

    pub(super) fn prearm_capture(
        &mut self,
        stream: rdif_audio::PcmStream,
        max_submissions: usize,
        deadline_ns: u64,
    ) -> Result<(), PcmError> {
        if self.queues_dropped || self.state != rdif_audio::PcmCardState::Ready {
            return Ok(());
        }
        if !matches!(
            self.session(stream).map(|session| session.state),
            Some(SessionState::Prepared | SessionState::Running)
        ) {
            return Ok(());
        }
        let mut submitted = 0;
        while submitted < max_submissions {
            let (index, data_cpu, header_cpu, status_cpu) = {
                let session = self.session_mut(stream)?;
                let Some(index) = find_rx_slot(session) else {
                    break;
                };
                (
                    index,
                    session.buffers.data[index].take(),
                    session.buffers.headers[index].take(),
                    session.buffers.statuses[index].take(),
                )
            };
            let Some(data_cpu) = data_cpu else {
                break;
            };
            let Some(header_cpu) = header_cpu else {
                let session = self.session_mut(stream)?;
                session.buffers.data[index] = Some(data_cpu);
                break;
            };
            let Some(status_cpu) = status_cpu else {
                let session = self.session_mut(stream)?;
                session.buffers.data[index] = Some(data_cpu);
                session.buffers.headers[index] = Some(header_cpu);
                break;
            };
            let data = data_cpu.prepare_for_device();
            let header = header_cpu.prepare_for_device();
            let status = status_cpu.prepare_for_device();
            let Some(inflight_index) = self.rx_inflight.iter().position(Option::is_none) else {
                let session = self.session_mut(stream)?;
                session.buffers.data[index] =
                    Some(data.complete_without_device().into_cpu_buffer());
                session.buffers.headers[index] =
                    Some(header.complete_without_device().into_cpu_buffer());
                session.buffers.statuses[index] =
                    Some(status.complete_without_device().into_cpu_buffer());
                return Err(PcmError::Again);
            };

            // SAFETY: all three owners move into `rx_inflight` after add and
            // remain valid until the exact used-ring token is observed.
            let (add_result, should_notify) = unsafe {
                let Some(rx) = self.rx.as_mut() else {
                    return Err(PcmError::BadState);
                };
                let header_slice =
                    slice::from_raw_parts(header.cpu_ptr().as_ptr(), header.len().get());
                let data_slice =
                    slice::from_raw_parts_mut(data.cpu_ptr().as_ptr(), data.len().get());
                let status_slice =
                    slice::from_raw_parts_mut(status.cpu_ptr().as_ptr(), status.len().get());
                (
                    rx.add(&[header_slice], &mut [data_slice, status_slice]),
                    rx.should_notify(),
                )
            };
            let queue_token = match add_result {
                Ok(token) => token,
                Err(error) => {
                    let session = self.session_mut(stream)?;
                    session.buffers.data[index] =
                        Some(data.complete_without_device().into_cpu_buffer());
                    session.buffers.headers[index] =
                        Some(header.complete_without_device().into_cpu_buffer());
                    session.buffers.statuses[index] =
                        Some(status.complete_without_device().into_cpu_buffer());
                    return Err(virtio_pcm_error(error));
                }
            };
            if should_notify {
                self.transport.notify(RX_QUEUE);
            }
            // SAFETY: the three owners remain reachable through `rx_inflight`.
            let (data, header, status) = unsafe {
                (
                    data.into_in_flight(),
                    header.into_in_flight(),
                    status.into_in_flight(),
                )
            };
            let token = self.allocate_io_token(stream);
            self.rx_inflight[inflight_index] = Some(RxInFlight {
                queue_token,
                token,
                pool_index: index,
                deadline_ns,
                data,
                header,
                status,
            });
            submitted += 1;
        }
        Ok(())
    }

    pub(super) fn service_tx(&mut self) -> Result<bool, PcmError> {
        if !self.has_completion_capacity() {
            return Ok(false);
        }
        let used_token = {
            let Some(queue_owner) = self.tx.as_mut() else {
                return Err(PcmError::BadState);
            };
            queue_owner.peek_used()
        };
        let Some(used_token) = used_token else {
            return Ok(false);
        };
        let Some(index) = self
            .tx_inflight
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|tx| tx.queue_token == used_token))
        else {
            self.request_reset(PcmError::Protocol);
            return Err(PcmError::Protocol);
        };
        let Some(tx) = self.tx_inflight[index].take() else {
            self.request_reset(PcmError::Protocol);
            return Err(PcmError::Protocol);
        };
        // SAFETY: the used-ring token names this exact descriptor chain. All
        // three DMA owners stay in `tx` until pop completes and unshares them.
        let used_len = unsafe {
            let header = slice::from_raw_parts(tx.header.cpu_ptr().as_ptr(), tx.header.len().get());
            let data = slice::from_raw_parts(tx.data.cpu_ptr().as_ptr(), tx.data.len().get());
            let status =
                slice::from_raw_parts_mut(tx.status.cpu_ptr().as_ptr(), tx.status.len().get());
            let Some(queue_owner) = self.tx.as_mut() else {
                return Err(PcmError::BadState);
            };
            queue_owner.pop_used(tx.queue_token, &[header, data], &mut [status])
        };
        let used_len = match used_len {
            Ok(len) => len,
            Err(error) => {
                self.tx_inflight[index] = Some(tx);
                self.request_reset(virtio_pcm_error(error));
                return Err(virtio_pcm_error(error));
            }
        };
        // SAFETY: the matching used entry was observed and the buffers are no
        // longer device-reachable.
        let (data, header, status) = unsafe {
            (
                tx.data.complete_after_quiesce(),
                tx.header.complete_after_quiesce(),
                tx.status.complete_after_quiesce(),
            )
        };
        let status = status.into_cpu_buffer();
        let status_bytes = status.as_slice_cpu();
        let parsed = pcm_status(&status_bytes[..PCM_STATUS_SIZE]);
        let config = self
            .session(tx.token.stream())
            .and_then(|session| session.config);
        let result = match (parsed, config) {
            (Some((ResponseStatus::Ok, latency)), Some(config)) => {
                if usize::try_from(used_len).ok() != Some(PCM_STATUS_SIZE)
                    || latency_oversized(latency, config)
                {
                    Err(PcmError::Protocol)
                } else {
                    Ok(())
                }
            }
            (Some((..)), Some(_)) => Err(PcmError::Device),
            _ => Err(PcmError::Protocol),
        };
        self.restore_playback_buffers(
            tx.token.stream(),
            tx.pool_index,
            data.into_cpu_buffer(),
            header.into_cpu_buffer(),
            status,
        );
        match result {
            Ok(()) => {
                if self.session(tx.token.stream()).is_some() {
                    let _ = self.push_completion(PcmCompletion::Playback {
                        token: tx.token,
                        frames: tx.frames,
                    });
                }
                Ok(true)
            }
            Err(error) => {
                self.request_reset(error);
                Err(error)
            }
        }
    }

    pub(super) fn service_rx(&mut self) -> Result<bool, PcmError> {
        if !self.has_completion_capacity() {
            return Ok(false);
        }
        let used_token = {
            let Some(queue_owner) = self.rx.as_mut() else {
                return Err(PcmError::BadState);
            };
            queue_owner.peek_used()
        };
        let Some(used_token) = used_token else {
            return Ok(false);
        };
        let Some(index) = self
            .rx_inflight
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|rx| rx.queue_token == used_token))
        else {
            self.request_reset(PcmError::Protocol);
            return Err(PcmError::Protocol);
        };
        let Some(rx) = self.rx_inflight[index].take() else {
            self.request_reset(PcmError::Protocol);
            return Err(PcmError::Protocol);
        };
        // SAFETY: the used-ring token names this exact descriptor chain. The
        // header stays device-readable and the data/status stay writable until
        // pop has observed that the device is done.
        let used_len = unsafe {
            let header = slice::from_raw_parts(rx.header.cpu_ptr().as_ptr(), rx.header.len().get());
            let data = slice::from_raw_parts_mut(rx.data.cpu_ptr().as_ptr(), rx.data.len().get());
            let status =
                slice::from_raw_parts_mut(rx.status.cpu_ptr().as_ptr(), rx.status.len().get());
            let Some(queue_owner) = self.rx.as_mut() else {
                return Err(PcmError::BadState);
            };
            queue_owner.pop_used(rx.queue_token, &[header], &mut [data, status])
        };
        let used_len = match used_len {
            Ok(len) => len,
            Err(error) => {
                self.rx_inflight[index] = Some(rx);
                self.request_reset(virtio_pcm_error(error));
                return Err(virtio_pcm_error(error));
            }
        };
        // SAFETY: the matching used entry was observed and all buffers are no
        // longer device-reachable.
        let (data, header, status) = unsafe {
            (
                rx.data.complete_after_quiesce(),
                rx.header.complete_after_quiesce(),
                rx.status.complete_after_quiesce(),
            )
        };
        let status = status.into_cpu_buffer();
        let status_bytes = status.as_slice_cpu();
        let parsed = pcm_status(&status_bytes[..PCM_STATUS_SIZE]);
        let session = self.session(rx.token.stream());
        let config = session.and_then(|session| session.config);
        let closing = session.is_some_and(|session| {
            matches!(
                session.state,
                SessionState::Releasing | SessionState::Released
            )
        });
        let result = validate_rx_completion(used_len, parsed, config, closing);
        match result {
            Ok(RxOutcome::Data {
                frame_bytes,
                data_len,
                latency,
            }) => {
                if let Ok(session) = self.session_mut(rx.token.stream()) {
                    session.buffers.headers[rx.pool_index] = Some(header.into_cpu_buffer());
                    session.buffers.statuses[rx.pool_index] = Some(status);
                }
                let frames = (data_len / frame_bytes) as u32;
                let data_buffer = data.into_cpu_buffer();
                if self.session(rx.token.stream()).is_some() {
                    // SAFETY: this used-ring entry was observed with status
                    // OK, the data length was validated, and the device no
                    // longer accesses this buffer.
                    // SAFETY: `self.id` is issued once for this card and is
                    // never reused; `data_buffer` came from `self.dma` and its
                    // used-ring completion above proved the device is done.
                    match unsafe {
                        let owner = PcmBufferOwner::new(self.id);
                        PcmBuffer::from_dma(data_buffer, owner, PcmDirection::Capture, data_len)
                    } {
                        Ok(buffer) => {
                            let _ = self.push_completion(PcmCompletion::Capture {
                                token: rx.token,
                                buffer,
                                frames,
                                latency_bytes: latency,
                            });
                        }
                        Err(error) => {
                            self.request_reset(error);
                            return Err(error);
                        }
                    }
                }
                Ok(true)
            }
            Ok(RxOutcome::Cancelled) => {
                if let Ok(session) = self.session_mut(rx.token.stream()) {
                    session.buffers.data[rx.pool_index] = Some(data.into_cpu_buffer());
                    session.buffers.headers[rx.pool_index] = Some(header.into_cpu_buffer());
                    session.buffers.statuses[rx.pool_index] = Some(status);
                }
                let _ = self.push_completion(PcmCompletion::CaptureCancelled { token: rx.token });
                Ok(true)
            }
            Err(error) => {
                if let Ok(session) = self.session_mut(rx.token.stream()) {
                    session.buffers.headers[rx.pool_index] = Some(header.into_cpu_buffer());
                    session.buffers.statuses[rx.pool_index] = Some(status);
                }
                self.request_reset(error);
                Err(error)
            }
        }
    }

    pub(super) fn service_events(&mut self, max_events: usize) -> Result<usize, PcmError> {
        let mut processed = 0;
        while processed < max_events {
            if !self.has_completion_capacity() {
                break;
            }
            let used_token = {
                let Some(queue_owner) = self.event.as_mut() else {
                    return Err(PcmError::BadState);
                };
                queue_owner.peek_used()
            };
            let Some(used_token) = used_token else {
                break;
            };
            let Some(index) = self.event_inflight.iter().position(|slot| {
                slot.as_ref()
                    .is_some_and(|event| event.queue_token == used_token)
            }) else {
                self.request_reset(PcmError::Protocol);
                return Err(PcmError::Protocol);
            };
            let Some(inflight) = self.event_inflight[index].take() else {
                self.request_reset(PcmError::Protocol);
                return Err(PcmError::Protocol);
            };
            // SAFETY: the used-ring token names this exact event buffer and
            // the owner remains in `inflight` until pop returns.
            let used_len = unsafe {
                let output = slice::from_raw_parts_mut(
                    inflight.buffer.cpu_ptr().as_ptr(),
                    inflight.buffer.len().get(),
                );
                let Some(queue_owner) = self.event.as_mut() else {
                    return Err(PcmError::BadState);
                };
                queue_owner.pop_used(inflight.queue_token, &[], &mut [output])
            };
            let used_len = match used_len {
                Ok(len) => len,
                Err(error) => {
                    self.event_inflight[index] = Some(inflight);
                    self.request_reset(virtio_pcm_error(error));
                    return Err(virtio_pcm_error(error));
                }
            };
            // SAFETY: the matching used entry was observed, so the device no
            // longer writes this event buffer.
            let buffer = unsafe { inflight.buffer.complete_after_quiesce() };
            let buffer = buffer.into_cpu_buffer();
            let bytes = buffer.as_slice_cpu();
            let parsed = if usize::try_from(used_len).ok() == Some(EVENT_SIZE) {
                event(&bytes[..EVENT_SIZE])
            } else {
                None
            };
            self.event_pool[inflight.pool_index] = Some(buffer);
            let Some((kind, data)) = parsed else {
                self.request_reset(PcmError::Protocol);
                return Err(PcmError::Protocol);
            };
            let kind = match kind {
                EVT_JACK_CONNECTED => PcmEventKind::JackConnected,
                EVT_JACK_DISCONNECTED => PcmEventKind::JackDisconnected,
                EVT_PCM_PERIOD_ELAPSED => PcmEventKind::PeriodElapsed,
                EVT_PCM_XRUN => PcmEventKind::Xrun,
                other => PcmEventKind::Unknown(other),
            };
            let _ = self.push_completion(PcmCompletion::Event(PcmEvent { kind, data }));
            processed += 1;
        }
        Ok(processed)
    }

    pub(super) fn io_timeouts(&mut self, now_ns: u64) {
        let tx_expired = self.tx_inflight.iter().any(|slot| {
            slot.as_ref().is_some_and(|tx| {
                tx.deadline_ns != 0
                    && now_ns > tx.deadline_ns
                    && self
                        .session(tx.token.stream())
                        .is_some_and(|session| session.state == SessionState::Running)
            })
        });
        let rx_expired = self.rx_inflight.iter().any(|slot| {
            slot.as_ref().is_some_and(|rx| {
                rx.deadline_ns != 0
                    && now_ns > rx.deadline_ns
                    && self
                        .session(rx.token.stream())
                        .is_some_and(|session| session.state == SessionState::Running)
            })
        });
        if (tx_expired || rx_expired) && self.state != rdif_audio::PcmCardState::Resetting {
            self.request_reset(PcmError::Timeout);
        }
    }
}

fn latency_oversized(latency: u32, config: PcmConfig) -> bool {
    let Some(buffer_bytes) = config.buffer_bytes() else {
        return true;
    };
    let Some(frame_bytes) = config.frame_bytes() else {
        return true;
    };
    usize::try_from(latency).map_or(true, |latency| {
        latency > buffer_bytes || !latency.is_multiple_of(frame_bytes)
    })
}

enum RxOutcome {
    Data {
        frame_bytes: usize,
        data_len: usize,
        latency: u32,
    },
    Cancelled,
}

fn validate_rx_completion(
    used_len: u32,
    parsed: Option<(ResponseStatus, u32)>,
    config: Option<PcmConfig>,
    closing: bool,
) -> Result<RxOutcome, PcmError> {
    let Some(config) = config else {
        return Err(PcmError::Protocol);
    };
    let Some((status, latency)) = parsed else {
        return Err(PcmError::Protocol);
    };
    let used_len = usize::try_from(used_len).map_err(|_| PcmError::Protocol)?;
    let data_len = used_len
        .checked_sub(PCM_STATUS_SIZE)
        .ok_or(PcmError::Protocol)?;
    let frame_bytes = config.frame_bytes().ok_or(PcmError::Protocol)?;
    let period_bytes = config.period_bytes().ok_or(PcmError::Protocol)?;
    if data_len > period_bytes || !data_len.is_multiple_of(frame_bytes) {
        return Err(PcmError::Protocol);
    }
    if closing && (status != ResponseStatus::Ok || data_len < period_bytes) {
        return Ok(RxOutcome::Cancelled);
    }
    if status != ResponseStatus::Ok {
        return Err(PcmError::Device);
    }
    if latency_oversized(latency, config) {
        return Err(PcmError::Protocol);
    }
    Ok(RxOutcome::Data {
        frame_bytes,
        data_len,
        latency,
    })
}
