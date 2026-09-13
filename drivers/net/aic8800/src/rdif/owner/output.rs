use alloc::{collections::VecDeque, sync::Arc, vec::Vec};

use rdif_eth::{DmaBuffer, RxCompletion, WifiControlProgress};
use ringbuf::traits::{Consumer, Observer, Producer};

use crate::{
    AicError, AicEvent, ControlRequest, TxToken,
    rdif::{
        device::{QueueOwnerPorts, WifiProgressSender, WifiProgressSignal},
        error::AicRdifError,
    },
};

/// Bounded output ownership and backpressure state for one AIC owner.
pub(super) struct OwnerOutputs {
    queues: QueueOwnerPorts,
    wifi_progress: WifiProgressSender,
    wifi_progress_signal: Arc<WifiProgressSignal>,
    tx_tokens: VecDeque<(TxToken, DmaBuffer)>,
    pending_tx_completion: Option<DmaBuffer>,
    pending_rx_frame: Option<Vec<u8>>,
    pending_rx_completion: Option<RxCompletion>,
    pending_wifi_progress: Option<Result<WifiControlProgress, AicError>>,
    terminal_error: Option<AicError>,
    /// A queue completion became visible to the runtime and needs another
    /// bounded poll to reclaim it.  This is separate from Wi-Fi control
    /// progress because data queues are consumed by the network runtime.
    queue_progress: bool,
    next_tx_token: u64,
    wifi_active: bool,
}

impl OwnerOutputs {
    pub(super) fn new(
        queues: QueueOwnerPorts,
        wifi_progress: WifiProgressSender,
        wifi_progress_signal: Arc<WifiProgressSignal>,
    ) -> Self {
        Self {
            queues,
            wifi_progress,
            wifi_progress_signal,
            tx_tokens: VecDeque::new(),
            pending_tx_completion: None,
            pending_rx_frame: None,
            pending_rx_completion: None,
            pending_wifi_progress: None,
            terminal_error: None,
            queue_progress: false,
            next_tx_token: 1,
            wifi_active: false,
        }
    }

    pub(super) fn begin_control(&mut self, request: &ControlRequest) {
        if !matches!(request, ControlRequest::Cancel) {
            self.wifi_active = true;
        }
    }

    pub(super) fn take_tx_frame(&mut self) -> Option<(TxToken, Vec<u8>)> {
        let buffer = self.queues.tx_submit.try_pop()?;
        let length = buffer.len();
        buffer.complete_for_cpu(length);
        let frame = buffer.read_with_cpu(length, |bytes| bytes.to_vec());
        let token = TxToken::new(self.next_tx_token);
        self.next_tx_token = self.next_tx_token.wrapping_add(1).max(1);
        self.tx_tokens.push_back((token, buffer));
        Some((token, frame))
    }

    pub(super) fn consume_event(&mut self, event: AicEvent) -> Result<bool, AicRdifError> {
        let blocked = match event {
            AicEvent::Started { .. } | AicEvent::Stopped => false,
            AicEvent::ControlComplete | AicEvent::ControlCancelled => {
                let blocked = !self.publish_wifi_progress(Ok(WifiControlProgress::Complete));
                self.wifi_active = false;
                blocked
            }
            AicEvent::ControlFailed(error) => {
                log::error!("[wifi] AIC control operation failed: {error}");
                let blocked = !self.publish_wifi_progress(Err(error));
                self.wifi_active = false;
                blocked
            }
            AicEvent::Receive(frame) => !self.publish_rx(frame)?,
            AicEvent::TransmitComplete(token) => !self.publish_tx_completion(token)?,
            AicEvent::Failed(error) => {
                let blocked = self.wifi_active && !self.publish_wifi_progress(Err(error.clone()));
                self.wifi_active = false;
                if blocked {
                    self.terminal_error = Some(error);
                    true
                } else {
                    return Err(error.into());
                }
            }
        };
        Ok(blocked)
    }

    pub(super) fn publish_wait_progress(&mut self, progress: WifiControlProgress) {
        let _ = self.publish_wifi_progress(Ok(progress));
    }

    pub(super) fn flush(&mut self) -> Result<bool, AicRdifError> {
        if let Some(buffer) = self.pending_tx_completion.take() {
            match self.queues.tx_complete.try_push(buffer) {
                Ok(()) => {
                    // The flag is consumed by `rearm_and_advance`, which
                    // schedules the queue runtime to reclaim the returned DMA
                    // token.
                    self.queue_progress = true;
                }
                Err(buffer) => self.pending_tx_completion = Some(buffer),
            }
        }
        if let Some(frame) = self.pending_rx_frame.take() {
            let _ = self.publish_rx(frame)?;
        }
        if let Some(completion) = self.pending_rx_completion.take() {
            match self.queues.rx_complete.try_push(completion) {
                Ok(()) => self.queue_progress = true,
                Err(completion) => self.pending_rx_completion = Some(completion),
            }
        }
        if let Some(progress) = self.pending_wifi_progress.take() {
            match self.wifi_progress.try_push(progress) {
                Ok(()) => self.wifi_progress_signal.publish(),
                Err(progress) => self.pending_wifi_progress = Some(progress),
            }
        }
        if self.has_pending() {
            return Ok(false);
        }
        if let Some(error) = self.terminal_error.take() {
            return Err(error.into());
        }
        Ok(true)
    }

    pub(super) fn has_pending(&self) -> bool {
        self.pending_tx_completion.is_some()
            || self.pending_rx_frame.is_some()
            || self.pending_rx_completion.is_some()
            || self.pending_wifi_progress.is_some()
    }

    pub(super) fn has_runnable_pending(&self) -> bool {
        self.pending_tx_completion.is_some()
            || self.pending_rx_completion.is_some()
            || self.pending_wifi_progress.is_some()
            || (self.pending_rx_frame.is_some() && !self.queues.rx_submit.is_empty())
    }

    pub(super) fn take_queue_progress(&mut self) -> bool {
        core::mem::take(&mut self.queue_progress)
    }

    fn publish_tx_completion(&mut self, token: TxToken) -> Result<bool, AicRdifError> {
        if self.pending_tx_completion.is_some() {
            return Ok(false);
        }
        let index = self
            .tx_tokens
            .iter()
            .position(|(candidate, _)| *candidate == token)
            .ok_or(AicError::CompletionMismatch)?;
        let (_, buffer) = self
            .tx_tokens
            .remove(index)
            .ok_or(AicError::CompletionMismatch)?;
        match self.queues.tx_complete.try_push(buffer) {
            Ok(()) => {
                self.queue_progress = true;
                Ok(true)
            }
            Err(buffer) => {
                self.pending_tx_completion = Some(buffer);
                Ok(false)
            }
        }
    }

    fn publish_rx(&mut self, frame: Vec<u8>) -> Result<bool, AicRdifError> {
        if frame.len() > self.queues.rx_frame_size {
            return Err(AicError::MalformedResponse.into());
        }
        if self.pending_rx_frame.is_some() || self.pending_rx_completion.is_some() {
            return Ok(false);
        }
        let Some(mut buffer) = self.queues.rx_submit.try_pop() else {
            self.pending_rx_frame = Some(frame);
            return Ok(false);
        };
        debug_assert!(frame.len() <= buffer.capacity());
        buffer.complete_for_cpu(buffer.capacity());
        buffer.write_with_cpu(|target| target[..frame.len()].copy_from_slice(&frame));
        let completion = RxCompletion {
            buffer,
            packet_len: frame.len(),
        };
        match self.queues.rx_complete.try_push(completion) {
            Ok(()) => {
                self.queue_progress = true;
                Ok(true)
            }
            Err(completion) => {
                self.pending_rx_completion = Some(completion);
                Ok(false)
            }
        }
    }

    fn publish_wifi_progress(&mut self, progress: Result<WifiControlProgress, AicError>) -> bool {
        if !self.wifi_active {
            return !self.wifi_active;
        }
        let terminal = matches!(progress, Ok(WifiControlProgress::Complete) | Err(_));
        if !terminal && self.pending_wifi_progress.is_some() {
            return false;
        }
        if terminal {
            // A wait/retry item is only advisory.  Once the owner has a
            // terminal result, retain that result even when the bounded
            // progress ring is still blocked by older wait notifications.
            self.pending_wifi_progress = None;
            log::info!("[wifi] control result queued for network runtime");
        }
        match self.wifi_progress.try_push(progress) {
            Ok(()) => {
                self.wifi_progress_signal.publish();
                true
            }
            Err(progress) => {
                self.pending_wifi_progress = Some(progress);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec;

    use rdif_eth::QueueConfig;

    use super::*;
    use crate::rdif::device::{WifiChannels, queues::queue_parts};

    #[test]
    fn full_wifi_progress_ring_retains_the_next_owner_event() {
        let (_, _, queues) = queue_parts(QueueConfig {
            dma_mask: u64::MAX,
            align: 4,
            buf_size: 2048,
            ring_size: 2,
        });
        let WifiChannels {
            requests_tx: _,
            requests_rx: _,
            progress_tx,
            mut progress_rx,
            progress_signal,
        } = WifiChannels::new();
        let mut outputs = OwnerOutputs::new(queues, progress_tx, progress_signal);
        outputs.begin_control(&ControlRequest::Scan { ssid: None });

        for _ in 0..8 {
            outputs.publish_wait_progress(WifiControlProgress::WaitForInterrupt);
        }
        outputs.publish_wait_progress(WifiControlProgress::RetryAt { deadline_nanos: 17 });

        assert!(outputs.has_pending());
        assert_eq!(
            progress_rx.try_pop(),
            Some(Ok(WifiControlProgress::WaitForInterrupt))
        );
        assert!(outputs.flush().unwrap());

        let mut observed_retry = false;
        while let Some(progress) = progress_rx.try_pop() {
            observed_retry |= matches!(
                progress,
                Ok(WifiControlProgress::RetryAt { deadline_nanos: 17 })
            );
        }
        assert!(observed_retry);
    }

    #[test]
    fn terminal_wifi_progress_supersedes_a_pending_wait() {
        let (_, _, queues) = queue_parts(QueueConfig {
            dma_mask: u64::MAX,
            align: 4,
            buf_size: 2048,
            ring_size: 2,
        });
        let WifiChannels {
            progress_tx,
            progress_signal,
            ..
        } = WifiChannels::new();
        let mut outputs = OwnerOutputs::new(queues, progress_tx, progress_signal);
        outputs.begin_control(&ControlRequest::Scan { ssid: None });

        for _ in 0..8 {
            outputs.publish_wait_progress(WifiControlProgress::WaitForInterrupt);
        }
        outputs.publish_wait_progress(WifiControlProgress::RetryAt { deadline_nanos: 17 });
        assert!(matches!(
            outputs.pending_wifi_progress,
            Some(Ok(WifiControlProgress::RetryAt { deadline_nanos: 17 }))
        ));

        assert!(outputs.consume_event(AicEvent::ControlComplete).unwrap());
        assert!(matches!(
            outputs.pending_wifi_progress,
            Some(Ok(WifiControlProgress::Complete))
        ));
    }

    #[test]
    fn unknown_transmit_completion_is_rejected() {
        let (_, _, queues) = queue_parts(QueueConfig {
            dma_mask: u64::MAX,
            align: 4,
            buf_size: 2048,
            ring_size: 2,
        });
        let WifiChannels {
            progress_tx,
            progress_signal,
            ..
        } = WifiChannels::new();
        let mut outputs = OwnerOutputs::new(queues, progress_tx, progress_signal);

        assert!(matches!(
            outputs.consume_event(AicEvent::TransmitComplete(TxToken::new(99))),
            Err(AicRdifError::Core(AicError::CompletionMismatch))
        ));
    }

    #[test]
    fn oversized_receive_frame_is_rejected_before_an_invalid_completion_is_published() {
        let (_, _, queues) = queue_parts(QueueConfig {
            dma_mask: u64::MAX,
            align: 4,
            buf_size: 2048,
            ring_size: 2,
        });
        let WifiChannels {
            progress_tx,
            progress_signal,
            ..
        } = WifiChannels::new();
        let mut outputs = OwnerOutputs::new(queues, progress_tx, progress_signal);

        assert!(matches!(
            outputs.consume_event(AicEvent::Receive(vec![0; 2049])),
            Err(AicRdifError::Core(AicError::MalformedResponse))
        ));
    }

    #[test]
    fn receive_frame_is_retained_until_an_rx_buffer_is_available() {
        let (_, _, queues) = queue_parts(QueueConfig {
            dma_mask: u64::MAX,
            align: 4,
            buf_size: 2048,
            ring_size: 2,
        });
        let WifiChannels {
            progress_tx,
            progress_signal,
            ..
        } = WifiChannels::new();
        let mut outputs = OwnerOutputs::new(queues, progress_tx, progress_signal);

        assert!(
            outputs
                .consume_event(AicEvent::Receive(vec![1, 2, 3]))
                .unwrap()
        );
        assert!(outputs.has_pending());
        assert!(!outputs.has_runnable_pending());
    }

    #[test]
    fn queue_progress_signal_is_consumed_once() {
        let (_, _, queues) = queue_parts(QueueConfig {
            dma_mask: u64::MAX,
            align: 4,
            buf_size: 2048,
            ring_size: 2,
        });
        let WifiChannels {
            progress_tx,
            progress_signal,
            ..
        } = WifiChannels::new();
        let mut outputs = OwnerOutputs::new(queues, progress_tx, progress_signal);

        // A completion publication must wake exactly one follow-up queue
        // poll; it must not leave the endpoint permanently runnable.
        outputs.queue_progress = true;
        assert!(outputs.take_queue_progress());
        assert!(!outputs.take_queue_progress());
    }
}
