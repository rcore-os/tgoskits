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

/// Outcome of handing one transmit completion back to the runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TxPublish {
    /// The buffer went back to the runtime.
    Published,
    /// The return ring is full, so the buffer is held for the next flush.
    Deferred,
    /// An earlier buffer is still held; this completion keeps its owner and
    /// waits for its turn.
    Waiting,
}

/// Bounded output ownership and backpressure state for one AIC owner.
pub(super) struct OwnerOutputs {
    queues: QueueOwnerPorts,
    wifi_progress: WifiProgressSender,
    wifi_progress_signal: Arc<WifiProgressSignal>,
    tx_tokens: VecDeque<(TxToken, DmaBuffer)>,
    pending_tx_completion: Option<DmaBuffer>,
    /// Completion IDs waiting for the return ring or for an earlier held buffer.
    pending_tx_tokens: VecDeque<TxToken>,
    pending_rx_frame: Option<Vec<u8>>,
    pending_rx_completion: Option<RxCompletion>,
    pending_wifi_progress: Option<Result<WifiControlProgress, AicError>>,
    terminal_error: Option<AicError>,
    /// A queue completion became visible to the runtime and needs another
    /// bounded poll to reclaim it. This is separate from Wi-Fi control
    /// progress because the data queues are consumed by the network runtime.
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
            pending_tx_tokens: VecDeque::new(),
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

    /// Takes a bounded prefix from the TX submit ring, preserving one token and
    /// one DMA owner for each frame returned.
    pub(super) fn take_tx_batch(&mut self, limit: usize) -> Vec<(TxToken, Vec<u8>)> {
        let capacity = self.queues.tx_submit.capacity().get();
        let mut batch = Vec::with_capacity(limit.min(capacity));
        while batch.len() < limit {
            let Some(frame) = self.take_tx_frame_inner() else {
                break;
            };
            batch.push(frame);
        }
        batch
    }

    fn take_tx_frame_inner(&mut self) -> Option<(TxToken, Vec<u8>)> {
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
            AicEvent::TransmitComplete(token) => match self.publish_tx_completion(token)? {
                TxPublish::Published => false,
                TxPublish::Deferred => true,
                TxPublish::Waiting => {
                    self.pending_tx_tokens.push_back(token);
                    true
                }
            },
            AicEvent::TransmitAggregateComplete(tokens) => {
                let mut blocked = false;
                let mut mismatch = false;
                for token in tokens {
                    match self.publish_tx_completion(token) {
                        Ok(TxPublish::Published) => {}
                        Ok(TxPublish::Deferred) => blocked = true,
                        Ok(TxPublish::Waiting) => {
                            self.pending_tx_tokens.push_back(token);
                            blocked = true;
                        }
                        Err(AicRdifError::Core(AicError::CompletionMismatch)) => {
                            mismatch = true;
                        }
                        Err(error) => return Err(error),
                    }
                }
                if mismatch {
                    return Err(AicError::CompletionMismatch.into());
                }
                blocked
            }
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
        while let Some(token) = self.pending_tx_tokens.front().copied() {
            match self.publish_tx_completion(token)? {
                TxPublish::Published | TxPublish::Deferred => {
                    self.pending_tx_tokens.pop_front();
                }
                TxPublish::Waiting => break,
            }
        }
        if let Some(buffer) = self.pending_tx_completion.take() {
            match self.queues.tx_complete.try_push(buffer) {
                Ok(()) => self.queue_progress = true,
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
            || !self.pending_tx_tokens.is_empty()
            || self.pending_rx_frame.is_some()
            || self.pending_rx_completion.is_some()
            || self.pending_wifi_progress.is_some()
    }

    pub(super) fn has_runnable_pending(&self) -> bool {
        self.pending_tx_completion.is_some()
            || !self.pending_tx_tokens.is_empty()
            || self.pending_rx_completion.is_some()
            || self.pending_wifi_progress.is_some()
            || (self.pending_rx_frame.is_some() && !self.queues.rx_submit.is_empty())
    }

    pub(super) fn take_queue_progress(&mut self) -> bool {
        core::mem::take(&mut self.queue_progress)
    }

    fn publish_tx_completion(&mut self, token: TxToken) -> Result<TxPublish, AicRdifError> {
        let index = self
            .tx_tokens
            .iter()
            .position(|(candidate, _)| *candidate == token)
            .ok_or(AicError::CompletionMismatch)?;
        if self.pending_tx_completion.is_some() {
            return Ok(TxPublish::Waiting);
        }
        let (_, buffer) = self
            .tx_tokens
            .remove(index)
            .ok_or(AicError::CompletionMismatch)?;
        match self.queues.tx_complete.try_push(buffer) {
            Ok(()) => {
                self.queue_progress = true;
                Ok(TxPublish::Published)
            }
            Err(buffer) => {
                self.pending_tx_completion = Some(buffer);
                Ok(TxPublish::Deferred)
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

    use rdif_eth::{ITxQueue, QueueConfig};

    use super::*;
    use crate::{
        TxAggregation,
        rdif::device::{WifiChannels, queues::queue_parts},
        rdif_test_support::dma_buffer,
    };

    fn outputs(
        ring_size: usize,
    ) -> (
        OwnerOutputs,
        crate::rdif::device::queues::AicTxQueue,
        crate::rdif::device::queues::AicRxQueue,
    ) {
        let (tx, rx, queues) = queue_parts(QueueConfig {
            dma_mask: u64::MAX,
            align: 4,
            buf_size: 2048,
            ring_size,
        });
        let WifiChannels {
            progress_tx,
            progress_signal,
            ..
        } = WifiChannels::new();
        (
            OwnerOutputs::new(queues, progress_tx, progress_signal),
            tx,
            rx,
        )
    }

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
        let (mut outputs, mut tx, _) = outputs(2);
        tx.submit(dma_buffer(60)).unwrap();
        assert_eq!(outputs.take_tx_batch(1).len(), 1);

        assert!(matches!(
            outputs.consume_event(AicEvent::TransmitComplete(TxToken::new(99))),
            Err(AicRdifError::Core(AicError::CompletionMismatch))
        ));
        assert_eq!(outputs.tx_tokens.len(), 1);
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
    fn bounded_batch_take_moves_real_buffers_and_limits_the_producer_batch() {
        let (mut outputs, mut tx, _) = outputs(4);
        tx.submit(dma_buffer(60)).unwrap();
        tx.submit(dma_buffer(61)).unwrap();
        tx.submit(dma_buffer(62)).unwrap();

        let batch = outputs.take_tx_batch(TxAggregation::new(2, 512).packets);

        assert_eq!(batch.len(), 2);
        assert_eq!(outputs.tx_tokens.len(), 2);
        assert_eq!(batch[0].1.len(), 60);
        assert_eq!(batch[1].1.len(), 61);
        assert_eq!(outputs.take_tx_batch(2).len(), 1);
    }

    #[test]
    fn full_return_ring_holds_the_overflow_until_the_ring_drains() {
        // The submit ring and the return ring are the same width, so the
        // producer hands over what it has room for and the owner collects it
        // before the next frames are submitted.
        let (mut outputs, mut tx, _) = outputs(2);
        let mut tokens = Vec::new();
        for length in [60, 61] {
            tx.submit(dma_buffer(length)).unwrap();
        }
        tokens.extend(outputs.take_tx_batch(2).iter().map(|(token, _)| *token));
        for length in [62, 63] {
            tx.submit(dma_buffer(length)).unwrap();
        }
        tokens.extend(outputs.take_tx_batch(2).iter().map(|(token, _)| *token));

        assert!(
            outputs
                .consume_event(AicEvent::TransmitAggregateComplete(tokens))
                .unwrap(),
            "a completion the return ring cannot hold blocks the owner"
        );
        assert_eq!(
            outputs.tx_tokens.len(),
            1,
            "the packet behind the held buffer keeps its owner"
        );
        assert_eq!(outputs.pending_tx_tokens.len(), 1, "and waits for its turn");
        assert!(outputs.pending_tx_completion.is_some());

        assert!(tx.reclaim().is_some());
        assert!(tx.reclaim().is_some());
        assert!(
            !outputs.flush().unwrap(),
            "the held buffer refills the ring and the waiting packet stays queued"
        );
        assert!(tx.reclaim().is_some());
        assert!(
            outputs.flush().unwrap(),
            "the waiting packet returns once the ring has room"
        );
        assert!(outputs.tx_tokens.is_empty());
        assert!(tx.reclaim().is_some());
        assert!(tx.reclaim().is_none());
    }

    #[test]
    fn aggregate_mismatch_returns_every_buffer_the_completion_named() {
        for full_return_ring in [false, true] {
            let (mut outputs, mut tx, _) = outputs(4);
            if full_return_ring {
                for length in [64, 65, 66, 67] {
                    tx.submit(dma_buffer(length)).unwrap();
                }
                let tokens = outputs
                    .take_tx_batch(4)
                    .iter()
                    .map(|(token, _)| *token)
                    .collect();
                assert!(
                    !outputs
                        .consume_event(AicEvent::TransmitAggregateComplete(tokens))
                        .unwrap()
                );
            }
            for length in [60, 61, 62] {
                tx.submit(dma_buffer(length)).unwrap();
            }
            let batch = outputs.take_tx_batch(3);
            let tokens: Vec<_> = batch.iter().map(|(token, _)| *token).collect();

            let result = outputs.consume_event(AicEvent::TransmitAggregateComplete(vec![
                tokens[0],
                TxToken::new(u64::MAX),
                tokens[2],
            ]));

            assert!(
                matches!(
                    result,
                    Err(AicRdifError::Core(AicError::CompletionMismatch))
                ),
                "unknown tokens must be rejected even when the return ring is full"
            );
            if full_return_ring {
                for _ in 0..4 {
                    assert!(tx.reclaim().is_some());
                }
                assert!(!outputs.flush().unwrap());
                assert!(outputs.flush().unwrap());
            }
            assert!(
                tx.reclaim().is_some(),
                "the first known packet was returned"
            );
            assert!(
                tx.reclaim().is_some(),
                "the known packet behind the unknown id was returned too"
            );
            assert!(tx.reclaim().is_none());
            assert_eq!(
                outputs.tx_tokens.len(),
                1,
                "the packet the completion never named keeps its buffer"
            );
            assert_eq!(outputs.tx_tokens.front().unwrap().0, tokens[1]);
        }
    }
}
