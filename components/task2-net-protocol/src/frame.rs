//! Versioned task-2 datagram framing.

use core::convert::TryFrom;

use thiserror::Error;

/// Wire magic at the beginning of every protocol datagram.
pub const FRAME_MAGIC: [u8; 4] = *b"T2N1";
/// Current wire protocol version.
pub const PROTOCOL_VERSION: u8 = 1;
/// Bytes occupied by the fixed frame header.
pub const FRAME_HEADER_LEN: usize = 28;
/// Largest application payload accepted by the protocol.
pub const MAX_PAYLOAD_LEN: usize = 1200;
/// Largest UDP datagram emitted by the protocol.
pub const MAX_DATAGRAM_LEN: usize = FRAME_HEADER_LEN + MAX_PAYLOAD_LEN;

const RELIABLE_FLAG: u16 = 1;
const KNOWN_FLAGS: u16 = RELIABLE_FLAG;
const CHECKSUM_OFFSET: usize = 24;

/// Stable identity for one protocol session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionId(u32);

impl SessionId {
    /// Creates a session identity.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the wire value.
    pub const fn get(self) -> u32 {
        self.0
    }
}

/// Nonzero sequence number used by reliable frames.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SequenceNumber(u32);

impl SequenceNumber {
    /// First sequence number in a session.
    pub const FIRST: Self = Self(1);
    /// Zero denotes the absence of a sequence or acknowledgement.
    pub const NONE: Self = Self(0);

    /// Creates a sequence number from its wire representation.
    pub const fn from_wire(value: u32) -> Self {
        Self(value)
    }

    /// Returns the wire value.
    pub const fn get(self) -> u32 {
        self.0
    }

    /// Returns the next nonzero sequence number, including wraparound.
    pub const fn next(self) -> Self {
        let next = self.0.wrapping_add(1);
        if next == 0 { Self::FIRST } else { Self(next) }
    }

    /// Returns the previous nonzero sequence number, including wraparound.
    pub const fn previous(self) -> Self {
        if self.0 <= 1 {
            Self(u32::MAX)
        } else {
            Self(self.0 - 1)
        }
    }
}

/// Application message carried by a frame.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageKind {
    /// Command from the control side to the managed side.
    Control   = 1,
    /// Managed-side state report.
    Status    = 2,
    /// Explicit protocol or application rejection.
    Error     = 3,
    /// Positive acknowledgement for one reliable sequence.
    Ack       = 4,
    /// Liveness message which is intentionally not retransmitted.
    Heartbeat = 5,
}

impl MessageKind {
    /// Returns whether the message requires acknowledgement and retransmission.
    pub const fn requires_reliability(self) -> bool {
        matches!(self, Self::Control | Self::Status)
    }
}

impl TryFrom<u8> for MessageKind {
    type Error = ParseError;

    fn try_from(value: u8) -> Result<Self, ParseError> {
        match value {
            1 => Ok(Self::Control),
            2 => Ok(Self::Status),
            3 => Ok(Self::Error),
            4 => Ok(Self::Ack),
            5 => Ok(Self::Heartbeat),
            _ => Err(ParseError::UnknownMessageKind(value)),
        }
    }
}

/// Protocol rejection reason carried by an `ERROR` frame.
#[repr(u16)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ErrorCode {
    /// No error. Only valid outside `ERROR` frames.
    None               = 0,
    /// A typed payload failed semantic validation.
    InvalidParameter   = 1,
    /// A reliable sequence arrived ahead of the expected sequence.
    OutOfOrder         = 2,
    /// The message kind or flag combination is unsupported.
    UnsupportedMessage = 3,
    /// The datagram belongs to another session.
    SessionMismatch    = 4,
}

impl TryFrom<u16> for ErrorCode {
    type Error = ParseError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::InvalidParameter),
            2 => Ok(Self::OutOfOrder),
            3 => Ok(Self::UnsupportedMessage),
            4 => Ok(Self::SessionMismatch),
            _ => Err(ParseError::UnknownErrorCode(value)),
        }
    }
}

/// Borrowed, validated protocol frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Frame<'a> {
    kind: MessageKind,
    session_id: SessionId,
    sequence: SequenceNumber,
    acknowledgement: SequenceNumber,
    error_code: ErrorCode,
    reliable: bool,
    payload: &'a [u8],
}

impl<'a> Frame<'a> {
    /// Creates a reliable `CONTROL` or `STATUS` frame.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::MessageMustNotBeReliable`] for non-reliable
    /// message kinds and [`EncodeError::MissingSequence`] for sequence zero.
    pub fn reliable(
        kind: MessageKind,
        session_id: SessionId,
        sequence: SequenceNumber,
        payload: &'a [u8],
    ) -> Result<Self, EncodeError> {
        if !kind.requires_reliability() {
            return Err(EncodeError::MessageMustNotBeReliable(kind));
        }
        if sequence == SequenceNumber::NONE {
            return Err(EncodeError::MissingSequence);
        }
        ensure_payload_len(payload.len())?;
        Ok(Self {
            kind,
            session_id,
            sequence,
            acknowledgement: SequenceNumber::NONE,
            error_code: ErrorCode::None,
            reliable: true,
            payload,
        })
    }

    /// Creates an acknowledgement frame.
    pub fn acknowledgement(session_id: SessionId, sequence: SequenceNumber) -> Self {
        Self {
            kind: MessageKind::Ack,
            session_id,
            sequence: SequenceNumber::NONE,
            acknowledgement: sequence,
            error_code: ErrorCode::None,
            reliable: false,
            payload: &[],
        }
    }

    /// Creates an error frame correlated with a received sequence.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError::MissingErrorCode`] for `ErrorCode::None` or
    /// [`EncodeError::PayloadTooLarge`] for an oversized diagnostic payload.
    pub fn error(
        session_id: SessionId,
        sequence: SequenceNumber,
        error_code: ErrorCode,
        diagnostic: &'a [u8],
    ) -> Result<Self, EncodeError> {
        if error_code == ErrorCode::None {
            return Err(EncodeError::MissingErrorCode);
        }
        ensure_payload_len(diagnostic.len())?;
        Ok(Self {
            kind: MessageKind::Error,
            session_id,
            sequence: SequenceNumber::NONE,
            acknowledgement: sequence,
            error_code,
            reliable: false,
            payload: diagnostic,
        })
    }

    /// Creates an unsequenced heartbeat frame.
    pub const fn heartbeat(session_id: SessionId, payload: &'a [u8]) -> Self {
        Self {
            kind: MessageKind::Heartbeat,
            session_id,
            sequence: SequenceNumber::NONE,
            acknowledgement: SequenceNumber::NONE,
            error_code: ErrorCode::None,
            reliable: false,
            payload,
        }
    }

    /// Parses and validates one complete UDP datagram.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] when framing, flags, lengths, checksum, or
    /// message-specific invariants are invalid.
    pub fn parse(datagram: &'a [u8]) -> Result<Self, ParseError> {
        parse_frame(datagram)
    }

    /// Encodes the frame into a caller-owned datagram buffer.
    ///
    /// # Errors
    ///
    /// Returns [`EncodeError`] if the payload or output buffer is invalid.
    pub fn encode(&self, output: &mut [u8]) -> Result<usize, EncodeError> {
        encode_frame(self, output)
    }

    /// Returns the message kind.
    pub const fn kind(&self) -> MessageKind {
        self.kind
    }

    /// Returns the session identity.
    pub const fn session_id(&self) -> SessionId {
        self.session_id
    }

    /// Returns the reliable sequence, or zero for unsequenced messages.
    pub const fn sequence(&self) -> SequenceNumber {
        self.sequence
    }

    /// Returns the acknowledged or rejected sequence.
    pub const fn acknowledgement_number(&self) -> SequenceNumber {
        self.acknowledgement
    }

    /// Returns the error reason carried by this frame.
    pub const fn error_code(&self) -> ErrorCode {
        self.error_code
    }

    /// Returns whether the reliable flag is set.
    pub const fn is_reliable(&self) -> bool {
        self.reliable
    }

    /// Returns the application payload.
    pub const fn payload(&self) -> &'a [u8] {
        self.payload
    }
}

/// Failure while encoding an outbound frame.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum EncodeError {
    /// Payload exceeds [`MAX_PAYLOAD_LEN`].
    #[error("payload length {actual} exceeds protocol maximum {maximum}")]
    PayloadTooLarge {
        /// Supplied payload length.
        actual: usize,
        /// Maximum accepted payload length.
        maximum: usize,
    },
    /// Caller-provided output buffer is too small.
    #[error("output buffer has {available} bytes but frame needs {needed}")]
    OutputTooSmall {
        /// Required encoded length.
        needed: usize,
        /// Available output length.
        available: usize,
    },
    /// A reliable message was constructed without a sequence number.
    #[error("reliable message requires a nonzero sequence number")]
    MissingSequence,
    /// A message kind that must not be reliable was passed to `Frame::reliable`.
    #[error("message kind {0:?} must not use reliable framing")]
    MessageMustNotBeReliable(MessageKind),
    /// An `ERROR` frame was constructed without an error reason.
    #[error("error frame requires a nonzero error code")]
    MissingErrorCode,
}

/// Failure while parsing an inbound frame.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum ParseError {
    /// Datagram is shorter than the fixed header.
    #[error("datagram has {actual} bytes but frame header needs {minimum}")]
    TooShort {
        /// Actual datagram length.
        actual: usize,
        /// Minimum header length.
        minimum: usize,
    },
    /// Datagram magic is not `T2N1`.
    #[error("invalid frame magic")]
    InvalidMagic,
    /// Datagram uses an unsupported protocol version.
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u8),
    /// Message kind has no defined wire meaning.
    #[error("unknown message kind {0}")]
    UnknownMessageKind(u8),
    /// Error code has no defined wire meaning.
    #[error("unknown error code {0}")]
    UnknownErrorCode(u16),
    /// Frame contains unknown flag bits.
    #[error("unknown frame flags {0:#x}")]
    UnknownFlags(u16),
    /// Header length and UDP datagram length disagree.
    #[error("declared payload length {declared} does not match datagram length {actual}")]
    LengthMismatch {
        /// Header-declared payload length.
        declared: usize,
        /// Actual payload bytes in the datagram.
        actual: usize,
    },
    /// Payload exceeds [`MAX_PAYLOAD_LEN`].
    #[error("payload length {actual} exceeds protocol maximum {maximum}")]
    PayloadTooLarge {
        /// Supplied payload length.
        actual: usize,
        /// Maximum accepted payload length.
        maximum: usize,
    },
    /// CRC32 did not match the datagram contents.
    #[error("frame checksum mismatch")]
    ChecksumMismatch,
    /// Reliable message is missing a sequence or has an invalid kind/flag combination.
    #[error("invalid reliability fields for message kind {0:?}")]
    InvalidReliability(MessageKind),
    /// ACK frame contains payload, sequence, error, or reliability state.
    #[error("invalid acknowledgement frame")]
    InvalidAcknowledgement,
    /// ERROR frame contains no error code or has invalid sequence/flag state.
    #[error("invalid error frame")]
    InvalidErrorFrame,
    /// Heartbeat contains sequence, acknowledgement, error, or reliability state.
    #[error("invalid heartbeat frame")]
    InvalidHeartbeat,
}

fn parse_frame(datagram: &[u8]) -> Result<Frame<'_>, ParseError> {
    if datagram.len() < FRAME_HEADER_LEN {
        return Err(ParseError::TooShort {
            actual: datagram.len(),
            minimum: FRAME_HEADER_LEN,
        });
    }
    if datagram[..4] != FRAME_MAGIC {
        return Err(ParseError::InvalidMagic);
    }
    if datagram[4] != PROTOCOL_VERSION {
        return Err(ParseError::UnsupportedVersion(datagram[4]));
    }

    let kind = MessageKind::try_from(datagram[5])?;
    let flags = read_u16(datagram, 6);
    if flags & !KNOWN_FLAGS != 0 {
        return Err(ParseError::UnknownFlags(flags));
    }
    let payload_len = usize::from(read_u16(datagram, 20));
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(ParseError::PayloadTooLarge {
            actual: payload_len,
            maximum: MAX_PAYLOAD_LEN,
        });
    }
    let actual_payload_len = datagram.len() - FRAME_HEADER_LEN;
    if payload_len != actual_payload_len {
        return Err(ParseError::LengthMismatch {
            declared: payload_len,
            actual: actual_payload_len,
        });
    }
    let expected_checksum = read_u32(datagram, CHECKSUM_OFFSET);
    if expected_checksum != frame_checksum(datagram) {
        return Err(ParseError::ChecksumMismatch);
    }

    let frame = Frame {
        kind,
        session_id: SessionId::new(read_u32(datagram, 8)),
        sequence: SequenceNumber::from_wire(read_u32(datagram, 12)),
        acknowledgement: SequenceNumber::from_wire(read_u32(datagram, 16)),
        error_code: ErrorCode::try_from(read_u16(datagram, 22))?,
        reliable: flags & RELIABLE_FLAG != 0,
        payload: &datagram[FRAME_HEADER_LEN..],
    };
    validate_message_fields(&frame)?;
    Ok(frame)
}

fn encode_frame(frame: &Frame<'_>, output: &mut [u8]) -> Result<usize, EncodeError> {
    ensure_payload_len(frame.payload.len())?;
    let encoded_len = FRAME_HEADER_LEN + frame.payload.len();
    if output.len() < encoded_len {
        return Err(EncodeError::OutputTooSmall {
            needed: encoded_len,
            available: output.len(),
        });
    }

    let output = &mut output[..encoded_len];
    output.fill(0);
    output[..4].copy_from_slice(&FRAME_MAGIC);
    output[4] = PROTOCOL_VERSION;
    output[5] = frame.kind as u8;
    write_u16(output, 6, if frame.reliable { RELIABLE_FLAG } else { 0 });
    write_u32(output, 8, frame.session_id.get());
    write_u32(output, 12, frame.sequence.get());
    write_u32(output, 16, frame.acknowledgement.get());
    write_u16(output, 20, frame.payload.len() as u16);
    write_u16(output, 22, frame.error_code as u16);
    output[FRAME_HEADER_LEN..].copy_from_slice(frame.payload);
    let checksum = frame_checksum(output);
    write_u32(output, CHECKSUM_OFFSET, checksum);
    Ok(encoded_len)
}

fn validate_message_fields(frame: &Frame<'_>) -> Result<(), ParseError> {
    match frame.kind {
        MessageKind::Control | MessageKind::Status => {
            if !frame.reliable
                || frame.sequence == SequenceNumber::NONE
                || frame.acknowledgement != SequenceNumber::NONE
                || frame.error_code != ErrorCode::None
            {
                return Err(ParseError::InvalidReliability(frame.kind));
            }
        }
        MessageKind::Ack => {
            if frame.reliable
                || frame.sequence != SequenceNumber::NONE
                || frame.acknowledgement == SequenceNumber::NONE
                || frame.error_code != ErrorCode::None
                || !frame.payload.is_empty()
            {
                return Err(ParseError::InvalidAcknowledgement);
            }
        }
        MessageKind::Error => {
            if frame.reliable
                || frame.sequence != SequenceNumber::NONE
                || frame.error_code == ErrorCode::None
            {
                return Err(ParseError::InvalidErrorFrame);
            }
        }
        MessageKind::Heartbeat => {
            if frame.reliable
                || frame.sequence != SequenceNumber::NONE
                || frame.acknowledgement != SequenceNumber::NONE
                || frame.error_code != ErrorCode::None
            {
                return Err(ParseError::InvalidHeartbeat);
            }
        }
    }
    Ok(())
}

fn ensure_payload_len(payload_len: usize) -> Result<(), EncodeError> {
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(EncodeError::PayloadTooLarge {
            actual: payload_len,
            maximum: MAX_PAYLOAD_LEN,
        });
    }
    Ok(())
}

fn frame_checksum(datagram: &[u8]) -> u32 {
    let mut checksum = u32::MAX;
    checksum = crc32_update(checksum, &datagram[..CHECKSUM_OFFSET]);
    checksum = crc32_update(checksum, &[0; 4]);
    checksum = crc32_update(checksum, &datagram[CHECKSUM_OFFSET + 4..]);
    !checksum
}

fn crc32_update(mut checksum: u32, bytes: &[u8]) -> u32 {
    for byte in bytes {
        checksum ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(checksum & 1);
            checksum = (checksum >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    checksum
}

fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_be_bytes([bytes[offset], bytes[offset + 1]])
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_be_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn write_u16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

fn write_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reliable_frame_round_trips() {
        let payload = b"control";
        let frame = Frame::reliable(
            MessageKind::Control,
            SessionId::new(7),
            SequenceNumber::FIRST,
            payload,
        )
        .unwrap();
        let mut encoded = [0; MAX_DATAGRAM_LEN];

        let len = frame.encode(&mut encoded).unwrap();
        let decoded = Frame::parse(&encoded[..len]).unwrap();

        assert_eq!(decoded, frame);
    }

    #[test]
    fn corrupted_payload_fails_checksum_validation() {
        let frame = Frame::heartbeat(SessionId::new(9), b"heartbeat");
        let mut encoded = [0; MAX_DATAGRAM_LEN];
        let len = frame.encode(&mut encoded).unwrap();
        encoded[len - 1] ^= 0x80;

        assert_eq!(
            Frame::parse(&encoded[..len]),
            Err(ParseError::ChecksumMismatch)
        );
    }

    #[test]
    fn parser_rejects_unknown_flags_after_checksum_is_recomputed() {
        let frame = Frame::heartbeat(SessionId::new(9), &[]);
        let mut encoded = [0; MAX_DATAGRAM_LEN];
        let len = frame.encode(&mut encoded).unwrap();
        write_u16(&mut encoded, 6, 0x8000);
        write_u32(&mut encoded, CHECKSUM_OFFSET, 0);
        let checksum = frame_checksum(&encoded[..len]);
        write_u32(&mut encoded, CHECKSUM_OFFSET, checksum);

        assert_eq!(
            Frame::parse(&encoded[..len]),
            Err(ParseError::UnknownFlags(0x8000))
        );
    }

    #[test]
    fn acknowledgement_requires_nonzero_sequence() {
        let frame = Frame::acknowledgement(SessionId::new(1), SequenceNumber::NONE);
        let mut encoded = [0; MAX_DATAGRAM_LEN];
        let len = frame.encode(&mut encoded).unwrap();

        assert_eq!(
            Frame::parse(&encoded[..len]),
            Err(ParseError::InvalidAcknowledgement)
        );
    }

    #[test]
    fn sequence_wrap_skips_reserved_zero() {
        assert_eq!(
            SequenceNumber::from_wire(u32::MAX).next(),
            SequenceNumber::FIRST
        );
        assert_eq!(
            SequenceNumber::FIRST.previous(),
            SequenceNumber::from_wire(u32::MAX)
        );
    }
}
