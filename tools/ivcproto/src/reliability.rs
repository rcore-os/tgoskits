//! Bounded per-peer reliability for UDP transports.

use thiserror::Error;

use crate::control::AckPayload;

/// Number of future packets remembered for duplicate and reorder detection.
pub const RECEIVE_WINDOW_BITS: u32 = 64;

/// Recently replaced sessions remembered to reject delayed packets.
pub const RETIRED_SESSION_CAPACITY: usize = 8;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReliabilityConfig {
    pub ack_timeout_us: u64,
    /// Retransmissions after the original transmission.
    pub max_retries: u8,
}

impl ReliabilityConfig {
    pub const fn new(ack_timeout_us: u64, max_retries: u8) -> Result<Self, ReliabilityError> {
        if ack_timeout_us == 0 {
            return Err(ReliabilityError::ZeroAckTimeout);
        }
        Ok(Self {
            ack_timeout_us,
            max_retries,
        })
    }
}

impl Default for ReliabilityConfig {
    fn default() -> Self {
        Self {
            ack_timeout_us: 100_000,
            max_retries: 3,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReceiverMetrics {
    pub accepted: u64,
    pub duplicates: u64,
    pub reordered: u64,
    pub outside_window: u64,
    pub session_resets: u64,
    pub session_rejections: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SenderMetrics {
    pub started: u64,
    pub acknowledged: u64,
    pub retransmissions: u64,
    pub timeouts: u64,
    pub unexpected_acks: u64,
    pub rtt_samples: u64,
    pub rtt_sum_us: u64,
    pub rtt_min_us: u64,
    pub rtt_max_us: u64,
}

impl SenderMetrics {
    pub fn average_rtt_us(self) -> Option<u64> {
        (self.rtt_samples != 0).then(|| self.rtt_sum_us / self.rtt_samples)
    }

    fn observe_rtt(&mut self, rtt_us: u64) {
        self.rtt_samples += 1;
        self.rtt_sum_us = self.rtt_sum_us.saturating_add(rtt_us);
        if self.rtt_samples == 1 {
            self.rtt_min_us = rtt_us;
            self.rtt_max_us = rtt_us;
        } else {
            self.rtt_min_us = self.rtt_min_us.min(rtt_us);
            self.rtt_max_us = self.rtt_max_us.max(rtt_us);
        }
    }
}

/// Result of observing a packet. Application side effects belong only in `New`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Delivery {
    NewSession { out_of_order: bool },
    New { out_of_order: bool },
    Duplicate,
    OutsideWindow,
    SessionRejected,
}

/// Fixed-memory duplicate/reordering window for one configured peer.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ReceiveWindow {
    session_id: Option<u32>,
    next_sequence: u32,
    received_mask: u64,
    retired_sessions: [u32; RETIRED_SESSION_CAPACITY],
    retired_session_count: usize,
    retired_session_cursor: usize,
    metrics: ReceiverMetrics,
}

impl ReceiveWindow {
    pub const fn new() -> Self {
        Self {
            session_id: None,
            next_sequence: 1,
            received_mask: 0,
            retired_sessions: [0; RETIRED_SESSION_CAPACITY],
            retired_session_count: 0,
            retired_session_cursor: 0,
            metrics: ReceiverMetrics {
                accepted: 0,
                duplicates: 0,
                reordered: 0,
                outside_window: 0,
                session_resets: 0,
                session_rejections: 0,
            },
        }
    }

    pub fn observe(
        &mut self,
        session_id: u32,
        sequence: u32,
    ) -> Result<Delivery, ReliabilityError> {
        validate_identifiers(session_id, sequence)?;
        let new_session = match self.session_id {
            None => true,
            Some(current) if current == session_id => false,
            Some(_) if sequence != 1 || self.is_retired(session_id) => {
                self.metrics.session_rejections += 1;
                return Ok(Delivery::SessionRejected);
            }
            Some(current) => {
                self.retire(current);
                self.metrics.session_resets += 1;
                true
            }
        };
        if new_session {
            self.session_id = Some(session_id);
            self.next_sequence = 1;
            self.received_mask = 0;
        }

        if sequence < self.next_sequence {
            self.metrics.duplicates += 1;
            return Ok(Delivery::Duplicate);
        }
        let offset = sequence - self.next_sequence;
        if offset >= RECEIVE_WINDOW_BITS {
            self.metrics.outside_window += 1;
            return Ok(Delivery::OutsideWindow);
        }

        let bit = 1u64 << offset;
        if self.received_mask & bit != 0 {
            self.metrics.duplicates += 1;
            return Ok(Delivery::Duplicate);
        }
        self.received_mask |= bit;
        self.metrics.accepted += 1;
        let out_of_order = offset != 0;
        if out_of_order {
            self.metrics.reordered += 1;
        }
        while self.received_mask & 1 != 0 {
            self.received_mask >>= 1;
            self.next_sequence = self
                .next_sequence
                .checked_add(1)
                .ok_or(ReliabilityError::SequenceExhausted)?;
        }
        if new_session {
            Ok(Delivery::NewSession { out_of_order })
        } else {
            Ok(Delivery::New { out_of_order })
        }
    }

    pub fn acknowledgement(&self, acknowledged_sequence: u32) -> AckPayload {
        AckPayload {
            acknowledged_sequence,
            next_expected_sequence: self.next_sequence,
            received_mask: self.received_mask as u32,
        }
    }

    pub const fn metrics(&self) -> ReceiverMetrics {
        self.metrics
    }

    pub const fn session_id(&self) -> Option<u32> {
        self.session_id
    }

    fn is_retired(&self, session_id: u32) -> bool {
        self.retired_sessions[..self.retired_session_count].contains(&session_id)
    }

    fn retire(&mut self, session_id: u32) {
        self.retired_sessions[self.retired_session_cursor] = session_id;
        self.retired_session_cursor = (self.retired_session_cursor + 1) % RETIRED_SESSION_CAPACITY;
        self.retired_session_count = (self.retired_session_count + 1).min(RETIRED_SESSION_CAPACITY);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingTransmission {
    sequence: u32,
    first_sent_us: u64,
    last_sent_us: u64,
    retries: u8,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryAction {
    Idle,
    Wait,
    Retransmit { sequence: u32 },
    TimedOut { sequence: u32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AckResult {
    Acknowledged { sequence: u32, rtt_us: u64 },
    Ignored,
}

/// Single-flight reliable sender. The caller retains the encoded datagram.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct StopAndWaitSender {
    session_id: u32,
    next_sequence: u32,
    pending: Option<PendingTransmission>,
    config: ReliabilityConfig,
    metrics: SenderMetrics,
}

impl StopAndWaitSender {
    pub fn new(session_id: u32, config: ReliabilityConfig) -> Result<Self, ReliabilityError> {
        if session_id == 0 {
            return Err(ReliabilityError::ZeroSessionId);
        }
        if config.ack_timeout_us == 0 {
            return Err(ReliabilityError::ZeroAckTimeout);
        }
        Ok(Self {
            session_id,
            next_sequence: 1,
            pending: None,
            config,
            metrics: SenderMetrics::default(),
        })
    }

    /// Reserves the next sequence when the original datagram is transmitted.
    pub fn begin(&mut self, now_us: u64) -> Result<u32, ReliabilityError> {
        if self.pending.is_some() {
            return Err(ReliabilityError::TransmissionAlreadyPending);
        }
        let sequence = self.next_sequence;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or(ReliabilityError::SequenceExhausted)?;
        self.pending = Some(PendingTransmission {
            sequence,
            first_sent_us: now_us,
            last_sent_us: now_us,
            retries: 0,
        });
        self.metrics.started += 1;
        Ok(sequence)
    }

    pub fn poll(&mut self, now_us: u64) -> Result<RetryAction, ReliabilityError> {
        let Some(mut pending) = self.pending else {
            return Ok(RetryAction::Idle);
        };
        let elapsed = now_us
            .checked_sub(pending.last_sent_us)
            .ok_or(ReliabilityError::ClockMovedBackward)?;
        if elapsed < self.config.ack_timeout_us {
            return Ok(RetryAction::Wait);
        }
        if pending.retries >= self.config.max_retries {
            self.pending = None;
            self.metrics.timeouts += 1;
            return Ok(RetryAction::TimedOut {
                sequence: pending.sequence,
            });
        }
        pending.retries += 1;
        pending.last_sent_us = now_us;
        self.pending = Some(pending);
        self.metrics.retransmissions += 1;
        Ok(RetryAction::Retransmit {
            sequence: pending.sequence,
        })
    }

    pub fn acknowledge(
        &mut self,
        session_id: u32,
        sequence: u32,
        now_us: u64,
    ) -> Result<AckResult, ReliabilityError> {
        let Some(pending) = self.pending else {
            self.metrics.unexpected_acks += 1;
            return Ok(AckResult::Ignored);
        };
        if session_id != self.session_id || sequence != pending.sequence {
            self.metrics.unexpected_acks += 1;
            return Ok(AckResult::Ignored);
        }
        let rtt_us = now_us
            .checked_sub(pending.first_sent_us)
            .ok_or(ReliabilityError::ClockMovedBackward)?;
        self.pending = None;
        self.metrics.acknowledged += 1;
        self.metrics.observe_rtt(rtt_us);
        Ok(AckResult::Acknowledged { sequence, rtt_us })
    }

    pub const fn session_id(&self) -> u32 {
        self.session_id
    }

    pub const fn pending_sequence(&self) -> Option<u32> {
        match self.pending {
            Some(pending) => Some(pending.sequence),
            None => None,
        }
    }

    pub const fn metrics(&self) -> SenderMetrics {
        self.metrics
    }
}

/// Complete bounded reliability state for one statically configured peer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReliablePeer {
    pub sender: StopAndWaitSender,
    pub receiver: ReceiveWindow,
}

impl ReliablePeer {
    pub fn new(session_id: u32, config: ReliabilityConfig) -> Result<Self, ReliabilityError> {
        Ok(Self {
            sender: StopAndWaitSender::new(session_id, config)?,
            receiver: ReceiveWindow::new(),
        })
    }
}

#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
pub enum ReliabilityError {
    #[error("session identifier zero is reserved")]
    ZeroSessionId,
    #[error("sequence zero is reserved")]
    ZeroSequence,
    #[error("ack timeout must be nonzero")]
    ZeroAckTimeout,
    #[error("a transmission is already awaiting acknowledgement")]
    TransmissionAlreadyPending,
    #[error("sequence space is exhausted; rotate to a new session")]
    SequenceExhausted,
    #[error("monotonic clock moved backward")]
    ClockMovedBackward,
}

fn validate_identifiers(session_id: u32, sequence: u32) -> Result<(), ReliabilityError> {
    if session_id == 0 {
        return Err(ReliabilityError::ZeroSessionId);
    }
    if sequence == 0 {
        return Err(ReliabilityError::ZeroSequence);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receive_window_delivers_reordered_packets_once() {
        let mut window = ReceiveWindow::new();
        assert_eq!(
            window.observe(7, 2),
            Ok(Delivery::NewSession { out_of_order: true })
        );
        assert_eq!(window.observe(7, 2), Ok(Delivery::Duplicate));
        assert_eq!(
            window.observe(7, 1),
            Ok(Delivery::New {
                out_of_order: false
            })
        );
        assert_eq!(window.acknowledgement(1).next_expected_sequence, 3);
        assert_eq!(
            window.metrics(),
            ReceiverMetrics {
                accepted: 2,
                duplicates: 1,
                reordered: 1,
                outside_window: 0,
                session_resets: 0,
                session_rejections: 0,
            }
        );
    }

    #[test]
    fn far_future_packet_does_not_grow_state() {
        let mut window = ReceiveWindow::new();
        assert_eq!(window.observe(7, 65), Ok(Delivery::OutsideWindow));
        assert_eq!(window.acknowledgement(65).next_expected_sequence, 1);
        assert_eq!(window.metrics().outside_window, 1);
    }

    #[test]
    fn new_session_clears_old_duplicate_window() {
        let mut window = ReceiveWindow::new();
        assert!(matches!(
            window.observe(7, 1),
            Ok(Delivery::NewSession { .. })
        ));
        assert!(matches!(
            window.observe(8, 1),
            Ok(Delivery::NewSession { .. })
        ));
        assert_eq!(window.metrics().session_resets, 1);
    }

    #[test]
    fn delayed_retired_session_cannot_replace_the_current_session() {
        let mut window = ReceiveWindow::new();
        assert!(matches!(
            window.observe(7, 1),
            Ok(Delivery::NewSession { .. })
        ));
        assert!(matches!(
            window.observe(8, 1),
            Ok(Delivery::NewSession { .. })
        ));

        let delayed = window.observe(7, 2).unwrap();
        assert_eq!(delayed, Delivery::SessionRejected);
        assert_eq!(window.session_id(), Some(8));
        assert_eq!(window.metrics().session_rejections, 1);
    }

    #[test]
    fn replacement_session_must_start_at_sequence_one() {
        let mut window = ReceiveWindow::new();
        assert!(matches!(
            window.observe(7, 1),
            Ok(Delivery::NewSession { .. })
        ));

        assert_eq!(window.observe(8, 2), Ok(Delivery::SessionRejected));
        assert_eq!(window.session_id(), Some(7));
    }

    #[test]
    fn sender_retries_to_cap_then_times_out() {
        let config = ReliabilityConfig::new(10, 2).unwrap();
        let mut sender = StopAndWaitSender::new(9, config).unwrap();
        assert_eq!(sender.begin(100), Ok(1));
        assert_eq!(sender.poll(109), Ok(RetryAction::Wait));
        assert_eq!(
            sender.poll(110),
            Ok(RetryAction::Retransmit { sequence: 1 })
        );
        assert_eq!(
            sender.poll(120),
            Ok(RetryAction::Retransmit { sequence: 1 })
        );
        assert_eq!(sender.poll(130), Ok(RetryAction::TimedOut { sequence: 1 }));
        assert_eq!(sender.metrics().retransmissions, 2);
        assert_eq!(sender.metrics().timeouts, 1);
    }

    #[test]
    fn unexpected_ack_does_not_complete_pending_transmission() {
        let mut sender = StopAndWaitSender::new(9, ReliabilityConfig::default()).unwrap();
        sender.begin(100).unwrap();
        assert_eq!(sender.acknowledge(9, 2, 120), Ok(AckResult::Ignored));
        assert_eq!(sender.pending_sequence(), Some(1));
        assert_eq!(
            sender.acknowledge(9, 1, 140),
            Ok(AckResult::Acknowledged {
                sequence: 1,
                rtt_us: 40
            })
        );
        assert_eq!(sender.metrics().average_rtt_us(), Some(40));
    }
}
