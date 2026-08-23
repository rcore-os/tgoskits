#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

//! Framing primitives shared by the Starry/Linux and RTOS guest endpoints.
//!
//! The transport remains an ordinary UDP or TCP socket. This crate only owns
//! the application wire format and validation; it does not access guest
//! memory, sockets, MMIO, or hypervisor services.

use core::convert::TryFrom;

/// Protocol magic (`GIPC`).
pub const MAGIC: u32 = 0x4749_5043;
/// Current wire-format version.
pub const VERSION: u8 = 1;
/// Serialized header size in bytes.
pub const HEADER_LEN: usize = 32;
/// Maximum application payload for one frame.
pub const MAX_PAYLOAD: usize = 1200;

/// Reliability policy for a request/response session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RetryPolicy {
    /// Time to wait for an ACK or response before retrying, in milliseconds.
    pub timeout_ms: u64,
    /// Maximum number of retransmissions after the first send.
    pub max_retries: u8,
}

impl RetryPolicy {
    /// Creates a bounded policy and rejects a zero timeout.
    pub const fn new(timeout_ms: u64, max_retries: u8) -> Option<Self> {
        if timeout_ms == 0 {
            None
        } else {
            Some(Self {
                timeout_ms,
                max_retries,
            })
        }
    }
}

/// State of one reliable request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PendingRequest {
    /// Sequence being acknowledged.
    pub sequence: u32,
    /// Timestamp of the most recent send.
    pub sent_at_ms: u64,
    /// Number of retransmissions already attempted.
    pub retries: u8,
}

/// Action required when polling a pending request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RetryAction {
    /// No deadline has elapsed.
    Wait,
    /// Retransmit the request and record the returned timestamp.
    Retransmit,
    /// The bounded retry budget is exhausted.
    TimedOut,
}

/// Result of observing a received sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReceiveSequence {
    /// The sequence advances the receive window.
    New,
    /// The sequence was already delivered and must not be executed again.
    Duplicate,
    /// The sequence is stale or out of order.
    OutOfOrder,
}

/// Small reliable-session state machine shared by UDP and TCP endpoints.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReliableSession {
    policy: RetryPolicy,
    pending: Option<PendingRequest>,
    last_received: Option<u32>,
}

impl ReliableSession {
    /// Creates an idle session.
    pub const fn new(policy: RetryPolicy) -> Self {
        Self {
            policy,
            pending: None,
            last_received: None,
        }
    }

    /// Begins tracking a reliable request.
    pub fn begin(&mut self, sequence: u32, now_ms: u64) -> bool {
        if self.pending.is_some() {
            return false;
        }
        self.pending = Some(PendingRequest {
            sequence,
            sent_at_ms: now_ms,
            retries: 0,
        });
        true
    }

    /// Confirms the pending request if the sequence matches.
    pub fn acknowledge(&mut self, sequence: u32) -> bool {
        if self
            .pending
            .is_some_and(|pending| pending.sequence == sequence)
        {
            self.pending = None;
            true
        } else {
            false
        }
    }

    /// Polls the retry deadline.
    pub fn poll_retry(&mut self, now_ms: u64) -> RetryAction {
        let Some(mut pending) = self.pending else {
            return RetryAction::Wait;
        };
        if now_ms.saturating_sub(pending.sent_at_ms) < self.policy.timeout_ms {
            return RetryAction::Wait;
        }
        if pending.retries >= self.policy.max_retries {
            self.pending = None;
            return RetryAction::TimedOut;
        }
        pending.retries = pending.retries.saturating_add(1);
        pending.sent_at_ms = now_ms;
        self.pending = Some(pending);
        RetryAction::Retransmit
    }

    /// Classifies a received sequence and advances the receive window for new data.
    pub fn observe(&mut self, sequence: u32) -> ReceiveSequence {
        match self.last_received {
            None => {
                self.last_received = Some(sequence);
                ReceiveSequence::New
            }
            Some(previous) if sequence == previous => ReceiveSequence::Duplicate,
            Some(previous) if sequence.wrapping_sub(previous) < (1 << 31) => {
                self.last_received = Some(sequence);
                ReceiveSequence::New
            }
            Some(_) => ReceiveSequence::OutOfOrder,
        }
    }
}

/// Application message kinds.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MessageType {
    /// Session greeting and capability negotiation.
    Hello     = 1,
    /// Requests a control-state transition.
    Control   = 2,
    /// Reports the current RTOS control state.
    Status    = 3,
    /// Reports a protocol or application failure.
    Error     = 4,
    /// Liveness probe.
    Heartbeat = 5,
    /// Acknowledges a reliable datagram.
    Ack       = 6,
}

impl TryFrom<u8> for MessageType {
    type Error = FrameError;

    fn try_from(value: u8) -> Result<Self, FrameError> {
        match value {
            1 => Ok(Self::Hello),
            2 => Ok(Self::Control),
            3 => Ok(Self::Status),
            4 => Ok(Self::Error),
            5 => Ok(Self::Heartbeat),
            6 => Ok(Self::Ack),
            _ => Err(FrameError::UnknownMessageType),
        }
    }
}

/// Protocol-level error code carried by an [`Error`] message.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum ErrorCode {
    /// No error.
    None               = 0,
    /// The peer does not support the requested version.
    UnsupportedVersion = 1,
    /// The frame length is invalid.
    InvalidLength      = 2,
    /// The checksum does not match.
    ChecksumMismatch   = 3,
    /// The sequence is stale or out of order.
    InvalidSequence    = 4,
    /// The message payload is invalid.
    InvalidPayload     = 5,
    /// The requested operation is not supported.
    UnsupportedMessage = 6,
    /// The peer is temporarily unavailable.
    Busy               = 7,
}

/// Header flags.
pub mod flags {
    /// The sender requires an acknowledgement.
    pub const ACK_REQUIRED: u16 = 1 << 0;
    /// The frame is an acknowledgement for a prior sequence.
    pub const IS_ACK: u16 = 1 << 1;
}

/// A decoded or to-be-encoded application header.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    /// Wire magic.
    pub magic: u32,
    /// Wire-format version.
    pub version: u8,
    /// Message kind.
    pub message_type: MessageType,
    /// Reliability flags.
    pub flags: u16,
    /// Header length in bytes.
    pub header_len: u16,
    /// Payload length in bytes.
    pub payload_len: u16,
    /// Monotonic message sequence.
    pub sequence: u32,
    /// Sender timestamp in nanoseconds, if available.
    pub timestamp_ns: u64,
    /// Application error code.
    pub error_code: ErrorCode,
    /// CRC-32 over the header with this field cleared and the payload.
    pub checksum: u32,
}

impl Header {
    /// Construct a header for a payload.
    pub const fn new(
        message_type: MessageType,
        flags: u16,
        payload_len: usize,
        sequence: u32,
        timestamp_ns: u64,
        error_code: ErrorCode,
    ) -> Option<Self> {
        if payload_len > MAX_PAYLOAD {
            return None;
        }
        Some(Self {
            magic: MAGIC,
            version: VERSION,
            message_type,
            flags,
            header_len: HEADER_LEN as u16,
            payload_len: payload_len as u16,
            sequence,
            timestamp_ns,
            error_code,
            checksum: 0,
        })
    }
}

/// Errors returned by frame construction and validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameError {
    /// The output buffer cannot hold the encoded frame.
    OutputTooSmall,
    /// The input is shorter than the fixed header.
    TruncatedHeader,
    /// The input does not contain the declared payload length.
    TruncatedPayload,
    /// The magic is not recognized.
    InvalidMagic,
    /// The version is unsupported.
    UnsupportedVersion,
    /// The header length is not the current fixed size.
    InvalidHeaderLength,
    /// The payload exceeds the protocol limit.
    PayloadTooLarge,
    /// The payload length does not match the input frame.
    PayloadLengthMismatch,
    /// The message type is unknown.
    UnknownMessageType,
    /// The error code is unknown.
    UnknownErrorCode,
    /// The CRC-32 does not match.
    ChecksumMismatch,
}

impl core::fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        let message = match self {
            Self::OutputTooSmall => "output buffer too small",
            Self::TruncatedHeader => "truncated protocol header",
            Self::TruncatedPayload => "truncated protocol payload",
            Self::InvalidMagic => "invalid protocol magic",
            Self::UnsupportedVersion => "unsupported protocol version",
            Self::InvalidHeaderLength => "invalid protocol header length",
            Self::PayloadTooLarge => "protocol payload too large",
            Self::PayloadLengthMismatch => "protocol payload length mismatch",
            Self::UnknownMessageType => "unknown protocol message type",
            Self::UnknownErrorCode => "unknown protocol error code",
            Self::ChecksumMismatch => "protocol checksum mismatch",
        };
        formatter.write_str(message)
    }
}

/// Encodes a frame into caller-provided storage.
pub fn encode_frame(
    header: Header,
    payload: &[u8],
    output: &mut [u8],
) -> Result<usize, FrameError> {
    if payload.len() > MAX_PAYLOAD {
        return Err(FrameError::PayloadTooLarge);
    }
    let frame_len = HEADER_LEN + payload.len();
    if output.len() < frame_len {
        return Err(FrameError::OutputTooSmall);
    }

    let mut encoded = header;
    encoded.magic = MAGIC;
    encoded.version = VERSION;
    encoded.header_len = HEADER_LEN as u16;
    encoded.payload_len = payload.len() as u16;
    encoded.checksum = 0;
    write_header(encoded, &mut output[..HEADER_LEN]);
    output[HEADER_LEN..frame_len].copy_from_slice(payload);
    encoded.checksum = crc32(&output[..frame_len]);
    write_header(encoded, &mut output[..HEADER_LEN]);
    Ok(frame_len)
}

/// Decodes and validates a complete frame.
pub fn decode_frame(input: &[u8]) -> Result<(Header, &[u8]), FrameError> {
    if input.len() < HEADER_LEN {
        return Err(FrameError::TruncatedHeader);
    }
    let header = read_header(&input[..HEADER_LEN])?;
    if input.len() < HEADER_LEN + header.payload_len as usize {
        return Err(FrameError::TruncatedPayload);
    }
    if input.len() != HEADER_LEN + header.payload_len as usize {
        return Err(FrameError::PayloadLengthMismatch);
    }
    let payload = &input[HEADER_LEN..];
    let expected = header.checksum;
    let mut checksum_header = header;
    checksum_header.checksum = 0;
    let mut bytes = [0u8; HEADER_LEN + MAX_PAYLOAD];
    write_header(checksum_header, &mut bytes[..HEADER_LEN]);
    bytes[HEADER_LEN..HEADER_LEN + payload.len()].copy_from_slice(payload);
    if crc32(&bytes[..HEADER_LEN + payload.len()]) != expected {
        return Err(FrameError::ChecksumMismatch);
    }
    Ok((header, payload))
}

fn write_header(header: Header, output: &mut [u8]) {
    output[0..4].copy_from_slice(&header.magic.to_be_bytes());
    output[4] = header.version;
    output[5] = header.message_type as u8;
    output[6..8].copy_from_slice(&header.flags.to_be_bytes());
    output[8..10].copy_from_slice(&header.header_len.to_be_bytes());
    output[10..12].copy_from_slice(&header.payload_len.to_be_bytes());
    output[12..16].copy_from_slice(&header.sequence.to_be_bytes());
    output[16..24].copy_from_slice(&header.timestamp_ns.to_be_bytes());
    output[24..26].copy_from_slice(&(header.error_code as u16).to_be_bytes());
    output[26..30].copy_from_slice(&header.checksum.to_be_bytes());
    output[30..32].fill(0);
}

fn read_header(input: &[u8]) -> Result<Header, FrameError> {
    let magic = u32::from_be_bytes(input[0..4].try_into().unwrap());
    if magic != MAGIC {
        return Err(FrameError::InvalidMagic);
    }
    let version = input[4];
    if version != VERSION {
        return Err(FrameError::UnsupportedVersion);
    }
    let message_type = MessageType::try_from(input[5])?;
    let header_len = u16::from_be_bytes(input[8..10].try_into().unwrap());
    if header_len as usize != HEADER_LEN {
        return Err(FrameError::InvalidHeaderLength);
    }
    let payload_len = u16::from_be_bytes(input[10..12].try_into().unwrap());
    if payload_len as usize > MAX_PAYLOAD {
        return Err(FrameError::PayloadTooLarge);
    }
    let error_code = match u16::from_be_bytes(input[24..26].try_into().unwrap()) {
        0 => ErrorCode::None,
        1 => ErrorCode::UnsupportedVersion,
        2 => ErrorCode::InvalidLength,
        3 => ErrorCode::ChecksumMismatch,
        4 => ErrorCode::InvalidSequence,
        5 => ErrorCode::InvalidPayload,
        6 => ErrorCode::UnsupportedMessage,
        7 => ErrorCode::Busy,
        _ => return Err(FrameError::UnknownErrorCode),
    };
    Ok(Header {
        magic,
        version,
        message_type,
        flags: u16::from_be_bytes(input[6..8].try_into().unwrap()),
        header_len,
        payload_len,
        sequence: u32::from_be_bytes(input[12..16].try_into().unwrap()),
        timestamp_ns: u64::from_be_bytes(input[16..24].try_into().unwrap()),
        error_code,
        checksum: u32::from_be_bytes(input[26..30].try_into().unwrap()),
    })
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffff;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_header_and_payload() {
        let header = Header::new(
            MessageType::Control,
            flags::ACK_REQUIRED,
            5,
            7,
            99,
            ErrorCode::None,
        )
        .unwrap();
        let mut output = [0u8; HEADER_LEN + MAX_PAYLOAD];
        let length = encode_frame(header, b"hello", &mut output).unwrap();
        let (decoded, payload) = decode_frame(&output[..length]).unwrap();
        assert_eq!(decoded.message_type, MessageType::Control);
        assert_eq!(decoded.sequence, 7);
        assert_eq!(decoded.timestamp_ns, 99);
        assert_eq!(payload, b"hello");
    }

    #[test]
    fn rejects_corrupted_payload() {
        let header = Header::new(MessageType::Status, 0, 3, 1, 0, ErrorCode::None).unwrap();
        let mut output = [0u8; HEADER_LEN + MAX_PAYLOAD];
        let length = encode_frame(header, b"abc", &mut output).unwrap();
        output[length - 1] ^= 1;
        assert_eq!(
            decode_frame(&output[..length]),
            Err(FrameError::ChecksumMismatch)
        );
    }

    #[test]
    fn rejects_trailing_bytes() {
        let header = Header::new(MessageType::Heartbeat, 0, 0, 1, 0, ErrorCode::None).unwrap();
        let mut output = [0u8; HEADER_LEN + MAX_PAYLOAD + 1];
        let length = encode_frame(header, &[], &mut output).unwrap();
        output[length] = 0;
        assert_eq!(
            decode_frame(&output[..length + 1]),
            Err(FrameError::PayloadLengthMismatch)
        );
    }

    #[test]
    fn retries_then_times_out_with_a_bounded_budget() {
        let policy = RetryPolicy::new(10, 1).unwrap();
        let mut session = ReliableSession::new(policy);
        assert!(session.begin(4, 100));
        assert_eq!(session.poll_retry(109), RetryAction::Wait);
        assert_eq!(session.poll_retry(110), RetryAction::Retransmit);
        assert_eq!(session.poll_retry(119), RetryAction::Wait);
        assert_eq!(session.poll_retry(120), RetryAction::TimedOut);
    }

    #[test]
    fn duplicate_and_out_of_order_requests_are_not_new_work() {
        let policy = RetryPolicy::new(1, 0).unwrap();
        let mut session = ReliableSession::new(policy);
        assert_eq!(session.observe(10), ReceiveSequence::New);
        assert_eq!(session.observe(10), ReceiveSequence::Duplicate);
        assert_eq!(session.observe(9), ReceiveSequence::OutOfOrder);
        assert_eq!(session.observe(11), ReceiveSequence::New);
    }
}
