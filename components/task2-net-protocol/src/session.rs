//! Stop-and-wait reliability and liveness state machine.

use thiserror::Error;

use crate::{
    ControlMessage, EncodeError, ErrorCode, Frame, HeartbeatMessage, MessageKind, ParseError,
    PayloadError, SequenceNumber, SessionId, StatusMessage,
};

/// Default retransmission and liveness policy used by the QEMU smoke tools.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    retransmit_after_ms: u64,
    max_retries: u8,
    heartbeat_interval_ms: u64,
    peer_timeout_ms: u64,
}

impl RetryPolicy {
    /// Creates a policy after checking ordering and nonzero timing invariants.
    ///
    /// # Errors
    ///
    /// Returns [`PolicyError`] when a duration is zero or the peer timeout is
    /// not strictly longer than the heartbeat interval.
    pub const fn new(
        retransmit_after_ms: u64,
        max_retries: u8,
        heartbeat_interval_ms: u64,
        peer_timeout_ms: u64,
    ) -> Result<Self, PolicyError> {
        if retransmit_after_ms == 0 || heartbeat_interval_ms == 0 || peer_timeout_ms == 0 {
            return Err(PolicyError::ZeroDuration);
        }
        if peer_timeout_ms <= heartbeat_interval_ms {
            return Err(PolicyError::PeerTimeoutNotLongerThanHeartbeat);
        }
        Ok(Self {
            retransmit_after_ms,
            max_retries,
            heartbeat_interval_ms,
            peer_timeout_ms,
        })
    }

    /// Retransmit timeout in milliseconds.
    pub const fn retransmit_after_ms(self) -> u64 {
        self.retransmit_after_ms
    }

    /// Maximum number of retransmissions after the first send.
    pub const fn max_retries(self) -> u8 {
        self.max_retries
    }

    /// Period between unsequenced heartbeat frames.
    pub const fn heartbeat_interval_ms(self) -> u64 {
        self.heartbeat_interval_ms
    }

    /// Time without a valid inbound frame before entering safe mode.
    pub const fn peer_timeout_ms(self) -> u64 {
        self.peer_timeout_ms
    }
}

/// Public endpoint state used by the safety monitor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndpointState {
    /// Protocol traffic is healthy.
    Active,
    /// A timeout, malformed payload, or remote error requires safe behavior.
    Safe,
}

/// Result of handling an inbound datagram.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveEvent<'a> {
    /// A new reliable command or status was accepted exactly once.
    Delivered { frame: Frame<'a> },
    /// The pending reliable frame was acknowledged.
    Acknowledged { sequence: SequenceNumber },
    /// An acknowledgement for the most recently completed frame arrived a
    /// second time, which is harmless after a retransmission race.
    DuplicateAcknowledgement { sequence: SequenceNumber },
    /// A previously delivered sequence was received again.
    Duplicate { sequence: SequenceNumber },
    /// A sequence arrived ahead of the next expected number.
    OutOfOrder {
        sequence: SequenceNumber,
        expected: SequenceNumber,
    },
    /// A typed payload failed validation and forced safe mode.
    InvalidPayload { error: PayloadError },
    /// The peer explicitly reported a protocol error.
    RemoteError {
        code: ErrorCode,
        sequence: SequenceNumber,
    },
    /// A valid heartbeat refreshed liveness.
    Heartbeat {
        /// Decoded sender uptime carried by the heartbeat.
        message: HeartbeatMessage,
    },
    /// A valid frame belonged to another session identity.
    SessionMismatch,
    /// The datagram could not be parsed and was ignored.
    Rejected { error: ParseError },
}

/// Result of a periodic endpoint poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PollEvent {
    /// No timer action was due.
    Idle,
    /// A pending reliable frame was retransmitted.
    Retransmit {
        sequence: SequenceNumber,
        attempt: u8,
    },
    /// Retries were exhausted and the endpoint entered safe mode.
    RetryExhausted { sequence: SequenceNumber },
    /// No valid peer frame arrived before the liveness deadline.
    HeartbeatTimeout,
    /// An unsequenced heartbeat was emitted.
    HeartbeatSent,
}

/// Encoded length returned by a queue operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Transmission {
    sequence: SequenceNumber,
    datagram_len: usize,
}

impl Transmission {
    /// Returns the reliable sequence represented by the datagram.
    pub const fn sequence(self) -> SequenceNumber {
        self.sequence
    }

    /// Returns the number of bytes to send from the caller's output buffer.
    pub const fn datagram_len(self) -> usize {
        self.datagram_len
    }
}

/// Result of an endpoint receive operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReceiveResult<'a> {
    /// State-machine event produced by the datagram.
    pub event: ReceiveEvent<'a>,
    /// Number of response bytes in the caller-provided output buffer.
    pub response_len: usize,
}

/// Result of a timer poll.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PollResult {
    /// Timer event.
    pub event: PollEvent,
    /// Number of response bytes in the caller-provided output buffer.
    pub datagram_len: usize,
}

/// Errors returned by endpoint operations.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum SessionError {
    /// A second reliable message cannot be queued while one is outstanding.
    #[error("a reliable frame is already pending")]
    ReliableFramePending,
    /// The requested message kind cannot be queued reliably.
    #[error("message kind {0:?} is not a reliable application message")]
    UnsupportedReliableKind(MessageKind),
    /// Typed payload validation failed before transmission.
    #[error("invalid outbound payload: {0}")]
    InvalidPayload(#[from] PayloadError),
    /// The caller-provided datagram buffer was too small.
    #[error("cannot encode endpoint response: {0}")]
    Encode(#[from] EncodeError),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct PendingFrame {
    kind: MessageKind,
    sequence: SequenceNumber,
    payload_len: usize,
    payload: [u8; crate::MAX_PAYLOAD_LEN],
    last_sent_ms: u64,
    retries: u8,
}

/// A single-session reliable endpoint.
///
/// The endpoint deliberately permits one outstanding reliable frame. This
/// stop-and-wait contract makes duplicate suppression and pcap accounting
/// deterministic while still exercising timeout and retransmission paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Endpoint {
    session_id: SessionId,
    policy: RetryPolicy,
    state: EndpointState,
    next_tx_sequence: SequenceNumber,
    last_acknowledged_tx_sequence: SequenceNumber,
    next_rx_sequence: SequenceNumber,
    pending: Option<PendingFrame>,
    last_rx_ms: u64,
    last_tx_ms: u64,
}

impl Endpoint {
    /// Creates a new endpoint with an initially active session.
    pub const fn new(session_id: SessionId, policy: RetryPolicy, now_ms: u64) -> Self {
        Self {
            session_id,
            policy,
            state: EndpointState::Active,
            next_tx_sequence: SequenceNumber::FIRST,
            last_acknowledged_tx_sequence: SequenceNumber::NONE,
            next_rx_sequence: SequenceNumber::FIRST,
            pending: None,
            last_rx_ms: now_ms,
            last_tx_ms: now_ms,
        }
    }

    /// Returns the endpoint session identity.
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the current safety state.
    pub const fn state(&self) -> EndpointState {
        self.state
    }

    /// Returns the next reliable sequence to be emitted.
    pub const fn next_tx_sequence(&self) -> SequenceNumber {
        self.next_tx_sequence
    }

    /// Returns the next reliable sequence expected from the peer.
    pub const fn next_rx_sequence(&self) -> SequenceNumber {
        self.next_rx_sequence
    }

    /// Returns whether a reliable frame is waiting for acknowledgement.
    pub const fn has_pending_frame(&self) -> bool {
        self.pending.is_some()
    }

    /// Queues and encodes a new reliable command or status.
    ///
    /// # Errors
    ///
    /// Returns an error if another frame is pending, the message kind is not
    /// reliable, its typed payload is invalid, or the output buffer is small.
    pub fn queue_reliable(
        &mut self,
        kind: MessageKind,
        payload: &[u8],
        now_ms: u64,
        output: &mut [u8],
    ) -> Result<Transmission, SessionError> {
        if self.pending.is_some() {
            return Err(SessionError::ReliableFramePending);
        }
        if !kind.requires_reliability() {
            return Err(SessionError::UnsupportedReliableKind(kind));
        }
        validate_typed_payload(kind, payload)?;

        let sequence = self.next_tx_sequence;
        let frame = Frame::reliable(kind, self.session_id, sequence, payload)?;
        let datagram_len = frame.encode(output)?;

        let mut stored_payload = [0; crate::MAX_PAYLOAD_LEN];
        stored_payload[..payload.len()].copy_from_slice(payload);
        self.pending = Some(PendingFrame {
            kind,
            sequence,
            payload_len: payload.len(),
            payload: stored_payload,
            last_sent_ms: now_ms,
            retries: 0,
        });
        self.next_tx_sequence = sequence.next();
        self.last_tx_ms = now_ms;
        Ok(Transmission {
            sequence,
            datagram_len,
        })
    }

    /// Handles one inbound datagram and optionally encodes an ACK or ERROR response.
    ///
    /// Malformed frames are returned as [`ReceiveEvent::Rejected`] and do not
    /// change endpoint state. Valid but semantically invalid payloads enter
    /// safe mode and produce an `ERROR` response.
    pub fn receive<'a>(
        &mut self,
        datagram: &'a [u8],
        now_ms: u64,
        output: &mut [u8],
    ) -> Result<ReceiveResult<'a>, SessionError> {
        let frame = match Frame::parse(datagram) {
            Ok(frame) => frame,
            Err(error) => {
                return Ok(ReceiveResult {
                    event: ReceiveEvent::Rejected { error },
                    response_len: 0,
                });
            }
        };
        if frame.session_id() != self.session_id {
            let response = Frame::error(
                self.session_id,
                frame.sequence(),
                ErrorCode::SessionMismatch,
                &[],
            )?;
            let response_len = response.encode(output)?;
            return Ok(ReceiveResult {
                event: ReceiveEvent::SessionMismatch,
                response_len,
            });
        }

        self.last_rx_ms = now_ms;
        let was_safe = self.state == EndpointState::Safe;
        self.state = EndpointState::Active;
        if was_safe {
            // The link was down long enough to enter safe mode, so the peer
            // may have dropped an unacknowledged reliable frame on retry
            // exhaustion and advanced its stream past our expectation (a lost
            // STATUS followed by a fresh CONTROL produces STATUS n+1 while we
            // still expect n).  Restart the whole reliable stream: heartbeats
            // are unsequenced, so recovering through one is safe, and the
            // next CONTROL/STATUS exchange then starts from FIRST on both
            // sides instead of looping on OutOfOrder forever.
            self.next_tx_sequence = SequenceNumber::FIRST;
            self.last_acknowledged_tx_sequence = SequenceNumber::NONE;
            self.next_rx_sequence = SequenceNumber::FIRST;
            self.pending = None;
        }
        match frame.kind() {
            MessageKind::Ack => self.handle_ack(frame),
            MessageKind::Control | MessageKind::Status => self.handle_reliable_frame(frame, output),
            MessageKind::Error => {
                self.state = EndpointState::Safe;
                Ok(ReceiveResult {
                    event: ReceiveEvent::RemoteError {
                        code: frame.error_code(),
                        sequence: frame.acknowledgement_number(),
                    },
                    response_len: 0,
                })
            }
            MessageKind::Heartbeat => match HeartbeatMessage::decode(frame.payload()) {
                Ok(message) => Ok(ReceiveResult {
                    event: ReceiveEvent::Heartbeat { message },
                    response_len: 0,
                }),
                Err(error) => {
                    self.state = EndpointState::Safe;
                    let response = Frame::error(
                        self.session_id,
                        frame.sequence(),
                        ErrorCode::InvalidParameter,
                        &[],
                    )?;
                    let response_len = response.encode(output)?;
                    Ok(ReceiveResult {
                        event: ReceiveEvent::InvalidPayload { error },
                        response_len,
                    })
                }
            },
        }
    }

    /// Runs retry and liveness timers and encodes any due outbound frame.
    pub fn poll(&mut self, now_ms: u64, output: &mut [u8]) -> Result<PollResult, SessionError> {
        if let Some(pending) = self.pending
            && now_ms.saturating_sub(pending.last_sent_ms) >= self.policy.retransmit_after_ms()
        {
            if pending.retries >= self.policy.max_retries() {
                self.pending = None;
                self.state = EndpointState::Safe;
                return Ok(PollResult {
                    event: PollEvent::RetryExhausted {
                        sequence: pending.sequence,
                    },
                    datagram_len: 0,
                });
            }
            let payload = &pending.payload[..pending.payload_len];
            let frame = Frame::reliable(pending.kind, self.session_id, pending.sequence, payload)?;
            let datagram_len = frame.encode(output)?;
            if let Some(current) = &mut self.pending {
                current.last_sent_ms = now_ms;
                current.retries = current.retries.saturating_add(1);
            }
            return Ok(PollResult {
                event: PollEvent::Retransmit {
                    sequence: pending.sequence,
                    attempt: pending.retries.saturating_add(1),
                },
                datagram_len,
            });
        }

        if self.state == EndpointState::Active
            && now_ms.saturating_sub(self.last_rx_ms) >= self.policy.peer_timeout_ms()
        {
            self.state = EndpointState::Safe;
            return Ok(PollResult {
                event: PollEvent::HeartbeatTimeout,
                datagram_len: 0,
            });
        }

        if now_ms.saturating_sub(self.last_tx_ms) >= self.policy.heartbeat_interval_ms() {
            let mut heartbeat_payload = [0; 8];
            let heartbeat = HeartbeatMessage::new(now_ms);
            heartbeat.encode(&mut heartbeat_payload)?;
            let frame = Frame::heartbeat(self.session_id, &heartbeat_payload);
            let datagram_len = frame.encode(output)?;
            self.last_tx_ms = now_ms;
            return Ok(PollResult {
                event: PollEvent::HeartbeatSent,
                datagram_len,
            });
        }

        Ok(PollResult {
            event: PollEvent::Idle,
            datagram_len: 0,
        })
    }

    fn handle_ack<'a>(&mut self, frame: Frame<'a>) -> Result<ReceiveResult<'a>, SessionError> {
        if let Some(pending) = self.pending
            && pending.sequence == frame.acknowledgement_number()
        {
            self.pending = None;
            self.last_acknowledged_tx_sequence = pending.sequence;
            return Ok(ReceiveResult {
                event: ReceiveEvent::Acknowledged {
                    sequence: pending.sequence,
                },
                response_len: 0,
            });
        }
        if frame.acknowledgement_number() == self.last_acknowledged_tx_sequence {
            return Ok(ReceiveResult {
                event: ReceiveEvent::DuplicateAcknowledgement {
                    sequence: frame.acknowledgement_number(),
                },
                response_len: 0,
            });
        }
        Ok(ReceiveResult {
            event: ReceiveEvent::Rejected {
                error: ParseError::InvalidAcknowledgement,
            },
            response_len: 0,
        })
    }

    fn handle_reliable_frame<'a>(
        &mut self,
        frame: Frame<'a>,
        output: &mut [u8],
    ) -> Result<ReceiveResult<'a>, SessionError> {
        if let Err(error) = validate_typed_payload(frame.kind(), frame.payload()) {
            self.state = EndpointState::Safe;
            let response = Frame::error(
                self.session_id,
                frame.sequence(),
                ErrorCode::InvalidParameter,
                &[],
            )?;
            let response_len = response.encode(output)?;
            return Ok(ReceiveResult {
                event: ReceiveEvent::InvalidPayload { error },
                response_len,
            });
        }

        if frame.sequence() == self.next_rx_sequence {
            self.next_rx_sequence = self.next_rx_sequence.next();
            let response = Frame::acknowledgement(self.session_id, frame.sequence());
            let response_len = response.encode(output)?;
            return Ok(ReceiveResult {
                event: ReceiveEvent::Delivered { frame },
                response_len,
            });
        }

        if frame.sequence() == self.next_rx_sequence.previous() {
            let response = Frame::acknowledgement(self.session_id, frame.sequence());
            let response_len = response.encode(output)?;
            return Ok(ReceiveResult {
                event: ReceiveEvent::Duplicate {
                    sequence: frame.sequence(),
                },
                response_len,
            });
        }

        self.state = EndpointState::Safe;
        let response = Frame::error(
            self.session_id,
            frame.sequence(),
            ErrorCode::OutOfOrder,
            &[],
        )?;
        let response_len = response.encode(output)?;
        Ok(ReceiveResult {
            event: ReceiveEvent::OutOfOrder {
                sequence: frame.sequence(),
                expected: self.next_rx_sequence,
            },
            response_len,
        })
    }
}

fn validate_typed_payload(kind: MessageKind, payload: &[u8]) -> Result<(), PayloadError> {
    match kind {
        MessageKind::Control => ControlMessage::decode(payload).map(|_| ()),
        MessageKind::Status => StatusMessage::decode(payload).map(|_| ()),
        MessageKind::Heartbeat => HeartbeatMessage::decode(payload).map(|_| ()),
        MessageKind::Error | MessageKind::Ack => Ok(()),
    }
}

/// Errors caused by an invalid retry or liveness configuration.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum PolicyError {
    /// A timer duration was zero.
    #[error("retry and heartbeat durations must be nonzero")]
    ZeroDuration,
    /// Peer timeout must leave room for heartbeat traffic.
    #[error("peer timeout must be longer than heartbeat interval")]
    PeerTimeoutNotLongerThanHeartbeat,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ControlAction, ControlMessage, Frame, MAX_DATAGRAM_LEN};

    const POLICY: RetryPolicy = match RetryPolicy::new(100, 2, 200, 600) {
        Ok(policy) => policy,
        Err(_) => panic!("test policy must be valid"),
    };

    fn control_payload() -> [u8; 12] {
        let mut payload = [0; 12];
        ControlMessage::new(ControlAction::SetOutput, 10, 1)
            .unwrap()
            .encode(&mut payload)
            .unwrap();
        payload
    }

    #[test]
    fn reliable_exchange_is_delivered_once_and_acknowledged() {
        let mut sender = Endpoint::new(SessionId::new(1), POLICY, 0);
        let mut receiver = Endpoint::new(SessionId::new(1), POLICY, 0);
        let payload = control_payload();
        let mut wire = [0; MAX_DATAGRAM_LEN];
        let mut response = [0; MAX_DATAGRAM_LEN];

        let transmission = sender
            .queue_reliable(MessageKind::Control, &payload, 1, &mut wire)
            .unwrap();
        let event = receiver
            .receive(&wire[..transmission.datagram_len()], 2, &mut response)
            .unwrap();
        assert!(matches!(event.event, ReceiveEvent::Delivered { .. }));
        assert!(!receiver.has_pending_frame());

        let ack_len = event.response_len;
        let ack_event = sender.receive(&response[..ack_len], 3, &mut wire).unwrap();
        assert_eq!(
            ack_event.event,
            ReceiveEvent::Acknowledged {
                sequence: SequenceNumber::FIRST
            }
        );
        assert!(!sender.has_pending_frame());

        let duplicate_ack_event = sender.receive(&response[..ack_len], 4, &mut wire).unwrap();
        assert_eq!(
            duplicate_ack_event.event,
            ReceiveEvent::DuplicateAcknowledgement {
                sequence: SequenceNumber::FIRST
            }
        );
    }

    #[test]
    fn duplicate_delivery_is_acked_without_replaying_application_data() {
        let mut receiver = Endpoint::new(SessionId::new(1), POLICY, 0);
        let payload = control_payload();
        let frame = Frame::reliable(
            MessageKind::Control,
            SessionId::new(1),
            SequenceNumber::FIRST,
            &payload,
        )
        .unwrap();
        let mut wire = [0; MAX_DATAGRAM_LEN];
        let len = frame.encode(&mut wire).unwrap();
        let mut response = [0; MAX_DATAGRAM_LEN];

        assert!(matches!(
            receiver
                .receive(&wire[..len], 1, &mut response)
                .unwrap()
                .event,
            ReceiveEvent::Delivered { .. }
        ));
        assert!(matches!(
            receiver
                .receive(&wire[..len], 2, &mut response)
                .unwrap()
                .event,
            ReceiveEvent::Duplicate { .. }
        ));
    }

    #[test]
    fn retry_exhaustion_enters_safe_state() {
        let mut endpoint = Endpoint::new(SessionId::new(1), POLICY, 0);
        let payload = control_payload();
        let mut wire = [0; MAX_DATAGRAM_LEN];
        endpoint
            .queue_reliable(MessageKind::Control, &payload, 0, &mut wire)
            .unwrap();

        assert!(matches!(
            endpoint.poll(100, &mut wire).unwrap().event,
            PollEvent::Retransmit { attempt: 1, .. }
        ));
        assert!(matches!(
            endpoint.poll(200, &mut wire).unwrap().event,
            PollEvent::Retransmit { attempt: 2, .. }
        ));
        assert_eq!(
            endpoint.poll(300, &mut wire).unwrap().event,
            PollEvent::RetryExhausted {
                sequence: SequenceNumber::FIRST
            }
        );
        assert_eq!(endpoint.state(), EndpointState::Safe);
    }

    #[test]
    fn reliable_retransmission_does_not_suppress_heartbeat() {
        const HEARTBEAT_POLICY: RetryPolicy = match RetryPolicy::new(150, 2, 200, 600) {
            Ok(policy) => policy,
            Err(_) => panic!("test policy must be valid"),
        };
        let mut endpoint = Endpoint::new(SessionId::new(1), HEARTBEAT_POLICY, 0);
        let payload = control_payload();
        let mut wire = [0; MAX_DATAGRAM_LEN];

        endpoint
            .queue_reliable(MessageKind::Control, &payload, 0, &mut wire)
            .unwrap();
        assert!(matches!(
            endpoint.poll(150, &mut wire).unwrap().event,
            PollEvent::Retransmit { attempt: 1, .. }
        ));
        assert_eq!(
            endpoint.poll(200, &mut wire).unwrap().event,
            PollEvent::HeartbeatSent
        );
        assert!(matches!(
            endpoint.poll(300, &mut wire).unwrap().event,
            PollEvent::Retransmit { attempt: 2, .. }
        ));
    }

    #[test]
    fn invalid_payload_forces_safe_mode_and_emits_error() {
        let mut receiver = Endpoint::new(SessionId::new(1), POLICY, 0);
        let frame = Frame::reliable(
            MessageKind::Control,
            SessionId::new(1),
            SequenceNumber::FIRST,
            &[1, 2, 3],
        )
        .unwrap();
        let mut wire = [0; MAX_DATAGRAM_LEN];
        let len = frame.encode(&mut wire).unwrap();
        let mut response = [0; MAX_DATAGRAM_LEN];

        let result = receiver.receive(&wire[..len], 1, &mut response).unwrap();
        assert!(matches!(result.event, ReceiveEvent::InvalidPayload { .. }));
        assert_eq!(receiver.state(), EndpointState::Safe);
        assert_eq!(
            Frame::parse(&response[..result.response_len])
                .unwrap()
                .error_code(),
            ErrorCode::InvalidParameter
        );
    }

    #[test]
    fn heartbeat_timeout_and_recovery_are_observable() {
        let mut endpoint = Endpoint::new(SessionId::new(1), POLICY, 0);
        let mut wire = [0; MAX_DATAGRAM_LEN];
        assert_eq!(
            endpoint.poll(600, &mut wire).unwrap().event,
            PollEvent::HeartbeatTimeout
        );
        assert_eq!(endpoint.state(), EndpointState::Safe);

        let heartbeat_payload = [0; 8];
        let heartbeat = Frame::heartbeat(SessionId::new(1), &heartbeat_payload);
        let len = heartbeat.encode(&mut wire).unwrap();
        let mut response = [0; MAX_DATAGRAM_LEN];
        assert_eq!(
            endpoint
                .receive(&wire[..len], 601, &mut response)
                .unwrap()
                .event,
            ReceiveEvent::Heartbeat {
                message: HeartbeatMessage::new(0),
            }
        );
        assert_eq!(endpoint.state(), EndpointState::Active);
    }

    #[test]
    fn invalid_heartbeat_forces_safe_mode_and_emits_error() {
        let mut endpoint = Endpoint::new(SessionId::new(1), POLICY, 0);
        let heartbeat = Frame::heartbeat(SessionId::new(1), &[0; 7]);
        let mut wire = [0; MAX_DATAGRAM_LEN];
        let len = heartbeat.encode(&mut wire).unwrap();
        let mut response = [0; MAX_DATAGRAM_LEN];

        let result = endpoint.receive(&wire[..len], 1, &mut response).unwrap();
        assert!(matches!(result.event, ReceiveEvent::InvalidPayload { .. }));
        assert_eq!(endpoint.state(), EndpointState::Safe);
        assert_eq!(
            Frame::parse(&response[..result.response_len])
                .unwrap()
                .error_code(),
            ErrorCode::InvalidParameter
        );
    }

    #[test]
    fn out_of_order_enters_safe_mode_and_reports_expected_sequence() {
        let mut receiver = Endpoint::new(SessionId::new(1), POLICY, 0);
        let payload = control_payload();
        let frame = Frame::reliable(
            MessageKind::Control,
            SessionId::new(1),
            SequenceNumber::from_wire(2),
            &payload,
        )
        .unwrap();
        let mut wire = [0; MAX_DATAGRAM_LEN];
        let len = frame.encode(&mut wire).unwrap();
        let mut response = [0; MAX_DATAGRAM_LEN];

        let result = receiver.receive(&wire[..len], 1, &mut response).unwrap();
        assert_eq!(
            result.event,
            ReceiveEvent::OutOfOrder {
                sequence: SequenceNumber::from_wire(2),
                expected: SequenceNumber::FIRST,
            }
        );
        assert_eq!(receiver.state(), EndpointState::Safe);
        let error = Frame::parse(&response[..result.response_len]).unwrap();
        assert_eq!(error.kind(), MessageKind::Error);
        assert_eq!(error.error_code(), ErrorCode::OutOfOrder);
    }

    #[test]
    fn recovery_resynchronises_reliable_stream_after_safe_mode() {
        let mut receiver = Endpoint::new(SessionId::new(1), POLICY, 0);
        let payload = control_payload();
        let mut wire = [0; MAX_DATAGRAM_LEN];
        let mut response = [0; MAX_DATAGRAM_LEN];

        for sequence in [SequenceNumber::FIRST, SequenceNumber::FIRST.next()] {
            let frame =
                Frame::reliable(MessageKind::Control, SessionId::new(1), sequence, &payload)
                    .unwrap();
            let len = frame.encode(&mut wire).unwrap();
            let result = receiver.receive(&wire[..len], 1, &mut response).unwrap();
            assert!(matches!(result.event, ReceiveEvent::Delivered { .. }));
        }
        assert_eq!(
            receiver.next_rx_sequence(),
            SequenceNumber::FIRST.next().next()
        );

        // The link drops after the managed side exhausted a lost STATUS and
        // already advanced to the next one (CONTROL n+1 -> STATUS n+1), while
        // the controller is still expecting the lost sequence.
        receiver.state = EndpointState::Safe;

        // A heartbeat after link recovery must restart both counters.
        let heartbeat = Frame::heartbeat(SessionId::new(1), &[0; 8]);
        let len = heartbeat.encode(&mut wire).unwrap();
        let result = receiver.receive(&wire[..len], 2, &mut response).unwrap();
        assert!(matches!(result.event, ReceiveEvent::Heartbeat { .. }));
        assert_eq!(receiver.state(), EndpointState::Active);
        assert_eq!(receiver.next_rx_sequence(), SequenceNumber::FIRST);
        assert_eq!(receiver.next_tx_sequence(), SequenceNumber::FIRST);
        assert!(!receiver.has_pending_frame());

        // The restarted stream accepts the peer's fresh sequence 1 instead of
        // rejecting it as OutOfOrder forever.
        let frame = Frame::reliable(
            MessageKind::Control,
            SessionId::new(1),
            SequenceNumber::FIRST,
            &payload,
        )
        .unwrap();
        let len = frame.encode(&mut wire).unwrap();
        let result = receiver.receive(&wire[..len], 3, &mut response).unwrap();
        assert!(matches!(result.event, ReceiveEvent::Delivered { .. }));
    }

    #[test]
    fn session_mismatch_is_rejected_with_error_without_changing_state() {
        let mut endpoint = Endpoint::new(SessionId::new(1), POLICY, 0);
        let frame = Frame::heartbeat(SessionId::new(2), &[0; 8]);
        let mut wire = [0; MAX_DATAGRAM_LEN];
        let len = frame.encode(&mut wire).unwrap();
        let mut response = [0; MAX_DATAGRAM_LEN];

        let result = endpoint.receive(&wire[..len], 1, &mut response).unwrap();
        assert_eq!(result.event, ReceiveEvent::SessionMismatch);
        assert_eq!(endpoint.state(), EndpointState::Active);
        assert_eq!(
            Frame::parse(&response[..result.response_len])
                .unwrap()
                .error_code(),
            ErrorCode::SessionMismatch
        );
        assert_eq!(
            Frame::parse(&response[..result.response_len])
                .unwrap()
                .acknowledgement_number(),
            SequenceNumber::NONE
        );
    }

    #[test]
    fn reliable_session_mismatch_correlates_rejected_sequence() {
        let mut endpoint = Endpoint::new(SessionId::new(1), POLICY, 0);
        let payload = control_payload();
        let frame = Frame::reliable(
            MessageKind::Control,
            SessionId::new(2),
            SequenceNumber::from_wire(7),
            &payload,
        )
        .unwrap();
        let mut wire = [0; MAX_DATAGRAM_LEN];
        let len = frame.encode(&mut wire).unwrap();
        let mut response = [0; MAX_DATAGRAM_LEN];

        let result = endpoint.receive(&wire[..len], 1, &mut response).unwrap();
        assert_eq!(result.event, ReceiveEvent::SessionMismatch);
        let error = Frame::parse(&response[..result.response_len]).unwrap();
        assert_eq!(error.error_code(), ErrorCode::SessionMismatch);
        assert_eq!(error.acknowledgement_number(), SequenceNumber::from_wire(7));
    }
}
