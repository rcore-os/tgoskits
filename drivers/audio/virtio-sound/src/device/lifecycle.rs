//! Reset, shutdown and service lifecycle.

use rdif_audio::{
    PcmCardState, PcmCompletion, PcmDirection, PcmError, PcmServiceBudget, PcmServiceReport,
};
use virtio_drivers::{
    Hal,
    transport::{DeviceStatus, Transport},
};

use super::{
    VirtioSound,
    session::{DeferredRelease, SessionState, release_slot},
};

impl<H: Hal, T: Transport> VirtioSound<H, T> {
    pub(super) fn request_reset(&mut self, error: PcmError) {
        self.fail_waiting_releases(error);
        self.error = Some(error);
        if !self.fault_signaled {
            self.fault_signaled = self.push_completion(PcmCompletion::Fault(error));
        }
        if self.state != PcmCardState::Stopped
            && self.state != PcmCardState::Resetting
            && self.state != PcmCardState::ShuttingDown
        {
            self.state = PcmCardState::Resetting;
            self.state_changed = true;
            self.transport.set_status(DeviceStatus::empty());
        }
    }

    fn service_reset(&mut self) -> bool {
        if self.state != PcmCardState::Resetting && self.state != PcmCardState::ShuttingDown {
            return false;
        }
        if !self.transport.get_status().is_empty() {
            return false;
        }
        self.finish_confirmed_stop();
        true
    }

    fn finish_confirmed_stop(&mut self) {
        if self.transport_stopped {
            return;
        }
        self.publish_deferred_terminal();
        self.transport_stopped = true;
        self.state = PcmCardState::Stopped;
        self.state_changed = true;

        if let Some(control) = self.control_pending.take() {
            // SAFETY: status zero confirms the device no longer accesses any
            // queue or buffer. Both DMA owners are therefore quiescent.
            unsafe {
                core::mem::drop(control.request.complete_after_quiesce());
                core::mem::drop(control.response.complete_after_quiesce());
            }
        }
        for slot in &mut self.tx_inflight {
            if let Some(tx) = slot.take() {
                // SAFETY: see the status-zero proof above.
                unsafe {
                    core::mem::drop(tx.data.complete_after_quiesce());
                    core::mem::drop(tx.header.complete_after_quiesce());
                    core::mem::drop(tx.status.complete_after_quiesce());
                }
            }
        }
        for slot in &mut self.rx_inflight {
            if let Some(rx) = slot.take() {
                // SAFETY: see the status-zero proof above.
                unsafe {
                    core::mem::drop(rx.data.complete_after_quiesce());
                    core::mem::drop(rx.header.complete_after_quiesce());
                    core::mem::drop(rx.status.complete_after_quiesce());
                }
            }
        }
        for slot in &mut self.event_inflight {
            if let Some(event) = slot.take() {
                // SAFETY: see the status-zero proof above.
                unsafe {
                    core::mem::drop(event.buffer.complete_after_quiesce());
                }
            }
        }
        self.control_request_pool = None;
        self.control_response_pool = None;
        for slot in &mut self.event_pool {
            *slot = None;
        }
        self.playback = None;
        self.capture = None;
        self.drop_queues();
        if !self.shutdown_signaled {
            self.shutdown_signaled =
                self.push_completion(PcmCompletion::Shutdown { confirmed: true });
        }
    }

    fn drop_queues(&mut self) {
        if self.queues_dropped {
            return;
        }
        // SAFETY: status zero has been observed, so the device no longer
        // accesses queue memory. Each ManuallyDrop is dropped exactly once.
        if let Some(mut queue) = self.control.take() {
            // SAFETY: status zero was observed for this queue's device.
            unsafe { core::mem::ManuallyDrop::drop(&mut queue) };
        }
        if let Some(mut queue) = self.event.take() {
            // SAFETY: status zero was observed for this queue's device.
            unsafe { core::mem::ManuallyDrop::drop(&mut queue) };
        }
        if let Some(mut queue) = self.tx.take() {
            // SAFETY: status zero was observed for this queue's device.
            unsafe { core::mem::ManuallyDrop::drop(&mut queue) };
        }
        if let Some(mut queue) = self.rx.take() {
            // SAFETY: status zero was observed for this queue's device.
            unsafe { core::mem::ManuallyDrop::drop(&mut queue) };
        }
        self.queues_dropped = true;
    }

    pub(super) fn fail_waiting_releases(&mut self, error: PcmError) {
        for slot in &mut self.deferred_release {
            if let Some(DeferredRelease::Waiting(token)) = *slot {
                *slot = Some(DeferredRelease::Terminal {
                    token,
                    result: Err(error),
                });
            }
        }
    }

    fn close_session(&mut self, direction: PcmDirection) {
        match direction {
            PcmDirection::Playback => self.playback = None,
            PcmDirection::Capture => self.capture = None,
        }
    }

    fn publish_deferred_terminal(&mut self) {
        for index in [0, 1] {
            let Some(DeferredRelease::Terminal { token, result }) = self.deferred_release[index]
            else {
                continue;
            };
            if !self.has_completion_capacity() {
                break;
            }
            self.deferred_release[index] = None;
            if !self.push_completion(PcmCompletion::Control { token, result }) {
                self.deferred_release[index] = Some(DeferredRelease::Terminal { token, result });
                break;
            }
        }
    }

    fn finish_release_completions(&mut self) {
        self.publish_deferred_terminal();
        for direction in [PcmDirection::Playback, PcmDirection::Capture] {
            let slot = release_slot(direction);
            let Some(DeferredRelease::Waiting(token)) = self.deferred_release[slot] else {
                continue;
            };
            if self.state != PcmCardState::Ready
                || self.direction_has_pending(direction)
                || !self.has_completion_capacity()
            {
                continue;
            }
            self.deferred_release[slot] = None;
            if self.push_completion(PcmCompletion::Control {
                token,
                result: Ok(()),
            }) {
                self.close_session(direction);
            } else {
                self.deferred_release[slot] = Some(DeferredRelease::Waiting(token));
            }
        }
        self.publish_deferred_terminal();
    }

    fn direction_has_pending(&self, direction: PcmDirection) -> bool {
        match direction {
            PcmDirection::Playback => self.tx_inflight.iter().any(|slot| {
                slot.as_ref()
                    .is_some_and(|tx| tx.token.direction() == direction)
            }),
            PcmDirection::Capture => self.rx_inflight.iter().any(|slot| {
                slot.as_ref()
                    .is_some_and(|rx| rx.token.direction() == direction)
            }),
        }
    }

    fn release_timeouts(&mut self, now_ns: u64) {
        let playback_expired = self.playback.as_ref().is_some_and(|session| {
            session
                .release_deadline_ns
                .is_some_and(|deadline| now_ns > deadline)
                && matches!(
                    session.state,
                    SessionState::Releasing | SessionState::Released
                )
                && self.direction_has_pending(PcmDirection::Playback)
        });
        let capture_expired = self.capture.as_ref().is_some_and(|session| {
            session
                .release_deadline_ns
                .is_some_and(|deadline| now_ns > deadline)
                && matches!(
                    session.state,
                    SessionState::Releasing | SessionState::Released
                )
                && self.direction_has_pending(PcmDirection::Capture)
        });
        if playback_expired || capture_expired {
            self.request_reset(PcmError::Timeout);
        }
    }

    fn flush_terminal_completions(&mut self) {
        self.publish_deferred_terminal();
        if self.state == PcmCardState::Stopped
            && !self.shutdown_signaled
            && self.has_completion_capacity()
        {
            self.shutdown_signaled =
                self.push_completion(PcmCompletion::Shutdown { confirmed: true });
        }
        if !self.fault_signaled
            && self.has_completion_capacity()
            && matches!(
                self.state,
                PcmCardState::Resetting | PcmCardState::ShuttingDown | PcmCardState::Stopped
            )
            && let Some(error) = self.error
        {
            self.fault_signaled = self.push_completion(PcmCompletion::Fault(error));
        }
    }

    pub(super) fn service_queues(
        &mut self,
        now_ns: u64,
        budget: PcmServiceBudget,
    ) -> PcmServiceReport {
        let mut report = PcmServiceReport {
            state: self.state,
            ..PcmServiceReport::default()
        };
        let changed_before = self.state_changed;
        self.state_changed = false;

        self.flush_terminal_completions();
        self.finish_release_completions();

        if matches!(
            self.state,
            PcmCardState::Resetting | PcmCardState::ShuttingDown
        ) {
            let _ = self.service_reset();
            if let Some(deadline) = self.shutdown_deadline_ns
                && now_ns > deadline
                && !self.transport_stopped
            {
                self.error = Some(PcmError::ResetPending);
            }
        }

        if budget.control_ops > 0
            && !matches!(
                self.state,
                PcmCardState::Resetting | PcmCardState::ShuttingDown | PcmCardState::Stopped
            )
        {
            match self.service_control(now_ns) {
                Ok(true) => report.control_completed = 1,
                Ok(false) => {}
                Err(error) => self.record_service_error(error),
            }
        }

        if !matches!(
            self.state,
            PcmCardState::Resetting | PcmCardState::ShuttingDown | PcmCardState::Stopped
        ) {
            for _ in 0..budget.playback_ops {
                match self.service_tx() {
                    Ok(true) => {
                        report.playback_completed = report.playback_completed.saturating_add(1);
                    }
                    Ok(false) => break,
                    Err(error) => {
                        self.record_service_error(error);
                        break;
                    }
                }
            }
            for _ in 0..budget.capture_ops {
                match self.service_rx() {
                    Ok(true) => {
                        report.capture_completed = report.capture_completed.saturating_add(1);
                    }
                    Ok(false) => break,
                    Err(error) => {
                        self.record_service_error(error);
                        break;
                    }
                }
            }
            let capture_stream = self.capture.as_ref().map(|session| session.stream);
            if let Some(stream) = capture_stream
                && self.has_completion_capacity()
            {
                match self.prearm_capture(stream, budget.capture_ops as usize, 0) {
                    Err(error) if error != PcmError::Again => {
                        self.record_service_error(error);
                    }
                    _ => {}
                }
            }
            let processed = match self.service_events(budget.event_ops as usize) {
                Ok(processed) => processed,
                Err(error) => {
                    self.record_service_error(error);
                    0
                }
            };
            report.events = processed as u8;
            let remaining_events = (budget.event_ops as usize).saturating_sub(processed);
            if self.has_completion_capacity()
                && let Err(error) = self.refill_events(remaining_events)
            {
                self.record_service_error(error);
            }
            self.io_timeouts(now_ns);
        }

        self.release_timeouts(now_ns);
        self.finish_release_completions();
        self.flush_terminal_completions();
        report.state = self.state;
        report.state_changed = changed_before || self.state_changed;
        self.state_changed = false;
        report.error = self.error;
        report.transport_stopped = self.transport_stopped;
        report.backpressure = !self.has_completion_capacity()
            || (self.control_pending.is_some() && self.completion_capacity() < 2);
        report
    }

    fn record_service_error(&mut self, error: PcmError) {
        if error != PcmError::Again && self.error.is_none() {
            self.error = Some(error);
        }
    }
}

impl<H: Hal, T: Transport> Drop for VirtioSound<H, T> {
    fn drop(&mut self) {
        if !self.transport_stopped {
            self.transport.set_status(DeviceStatus::empty());
            // One status read is not a wait. If zero is not observed, queues
            // and in-flight DMA remain quarantined: ManuallyDrop and
            // InFlightDma intentionally do not release their backing here.
            if !self.transport.get_status().is_empty() {
                return;
            }
        }
        if !self.transport_stopped {
            self.finish_confirmed_stop();
        } else if !self.queues_dropped {
            self.drop_queues();
        }
        // SAFETY: this Drop runs once. Confirmed status zero means queue and
        // transport state are quiescent; an unconfirmed Drop returned above
        // and intentionally leaves the ManuallyDrop transport alive.
        unsafe { core::mem::ManuallyDrop::drop(&mut self.transport) };
    }
}
