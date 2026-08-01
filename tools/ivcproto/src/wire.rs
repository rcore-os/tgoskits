//! Versioned and checksummed IVC wire frames.

use core::fmt;

use thiserror::Error;

/// Four-byte marker chosen to be readable in packet captures.
pub const MAGIC: [u8; 4] = *b"IVC1";
/// Current wire-format version.
pub const VERSION: u8 = 1;
/// Fixed header size in bytes.
pub const HEADER_LEN: usize = 32;
/// Maximum application payload, keeping a frame below a conventional UDP MTU.
pub const MAX_PAYLOAD_LEN: usize = 1_200;
/// Offset of the CRC field inside the fixed header.
const CHECKSUM_OFFSET: usize = 28;

/// Application message carried by a frame.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum MessageType {
    Control   = 1,
    Status    = 2,
    Error     = 3,
    Ack       = 4,
    Telemetry = 5,
}

impl TryFrom<u8> for MessageType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, ProtocolError> {
        match value {
            1 => Ok(Self::Control),
            2 => Ok(Self::Status),
            3 => Ok(Self::Error),
            4 => Ok(Self::Ack),
            5 => Ok(Self::Telemetry),
            other => Err(ProtocolError::UnsupportedMessageType(other)),
        }
    }
}

/// Machine-readable result carried in every header.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(u16)]
pub enum ErrorCode {
    #[default]
    None               = 0,
    MalformedFrame     = 1,
    UnsupportedVersion = 2,
    ChecksumMismatch   = 3,
    SequenceOutsideWindow = 4,
    InvalidControl     = 5,
    StaleControl       = 6,
    ActuatorRange      = 7,
    ControllerTimeout  = 8,
    Internal           = 9,
}

impl TryFrom<u16> for ErrorCode {
    type Error = ProtocolError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::None),
            1 => Ok(Self::MalformedFrame),
            2 => Ok(Self::UnsupportedVersion),
            3 => Ok(Self::ChecksumMismatch),
            4 => Ok(Self::SequenceOutsideWindow),
            5 => Ok(Self::InvalidControl),
            6 => Ok(Self::StaleControl),
            7 => Ok(Self::ActuatorRange),
            8 => Ok(Self::ControllerTimeout),
            9 => Ok(Self::Internal),
            other => Err(ProtocolError::UnsupportedErrorCode(other)),
        }
    }
}

/// Flags describing delivery behavior.
#[derive(Clone, Copy, Default, Eq, PartialEq)]
pub struct FrameFlags(u16);

impl FrameFlags {
    pub const ACK_REQUIRED: Self = Self(1 << 0);
    pub const RETRANSMISSION: Self = Self(1 << 1);

    pub const fn empty() -> Self {
        Self(0)
    }

    pub const fn bits(self) -> u16 {
        self.0
    }

    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    fn from_bits(bits: u16) -> Result<Self, ProtocolError> {
        let known = Self::ACK_REQUIRED.0 | Self::RETRANSMISSION.0;
        if bits & !known != 0 {
            return Err(ProtocolError::UnsupportedFlags(bits));
        }
        Ok(Self(bits))
    }
}

impl fmt::Debug for FrameFlags {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FrameFlags")
            .field("ack_required", &self.contains(Self::ACK_REQUIRED))
            .field("retransmission", &self.contains(Self::RETRANSMISSION))
            .finish()
    }
}

/// Parsed fixed header. All multi-byte fields are little-endian on the wire.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Header {
    pub message_type: MessageType,
    pub flags: FrameFlags,
    pub session_id: u32,
    pub sequence: u32,
    pub timestamp_us: u64,
    pub error: ErrorCode,
}

impl Header {
    pub const fn new(
        message_type: MessageType,
        session_id: u32,
        sequence: u32,
        timestamp_us: u64,
    ) -> Self {
        Self {
            message_type,
            flags: FrameFlags::empty(),
            session_id,
            sequence,
            timestamp_us,
            error: ErrorCode::None,
        }
    }
}

/// Borrowed decoded datagram.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Frame<'a> {
    pub header: Header,
    pub payload: &'a [u8],
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ProtocolError {
    #[error("frame is shorter than the {HEADER_LEN}-byte header: {actual} bytes")]
    HeaderTooShort { actual: usize },
    #[error("invalid frame magic")]
    InvalidMagic,
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u8),
    #[error("unsupported message type {0}")]
    UnsupportedMessageType(u8),
    #[error("unsupported frame flags 0x{0:04x}")]
    UnsupportedFlags(u16),
    #[error("unsupported error code {0}")]
    UnsupportedErrorCode(u16),
    #[error("payload length {actual} exceeds the maximum {maximum}")]
    PayloadTooLarge { actual: usize, maximum: usize },
    #[error("output buffer requires {required} bytes but has {actual}")]
    OutputTooSmall { required: usize, actual: usize },
    #[error("declared frame length {declared} does not match datagram length {actual}")]
    LengthMismatch { declared: usize, actual: usize },
    #[error("frame checksum mismatch: expected 0x{expected:08x}, received 0x{received:08x}")]
    ChecksumMismatch { expected: u32, received: u32 },
    #[error("non-error frame carries error code {0:?}")]
    UnexpectedErrorCode(ErrorCode),
    #[error("error frame must carry a nonzero error code")]
    MissingErrorCode,
}

/// Encodes one complete UDP datagram into caller-owned storage.
pub fn encode_frame(
    header: Header,
    payload: &[u8],
    output: &mut [u8],
) -> Result<usize, ProtocolError> {
    validate_header_error(header)?;
    if payload.len() > MAX_PAYLOAD_LEN {
        return Err(ProtocolError::PayloadTooLarge {
            actual: payload.len(),
            maximum: MAX_PAYLOAD_LEN,
        });
    }
    let frame_len = HEADER_LEN + payload.len();
    if output.len() < frame_len {
        return Err(ProtocolError::OutputTooSmall {
            required: frame_len,
            actual: output.len(),
        });
    }

    let frame = &mut output[..frame_len];
    frame.fill(0);
    frame[..4].copy_from_slice(&MAGIC);
    frame[4] = VERSION;
    frame[5] = header.message_type as u8;
    frame[6..8].copy_from_slice(&header.flags.bits().to_le_bytes());
    frame[8..12].copy_from_slice(&header.session_id.to_le_bytes());
    frame[12..16].copy_from_slice(&header.sequence.to_le_bytes());
    frame[16..24].copy_from_slice(&header.timestamp_us.to_le_bytes());
    frame[24..26].copy_from_slice(&(payload.len() as u16).to_le_bytes());
    frame[26..28].copy_from_slice(&(header.error as u16).to_le_bytes());
    frame[HEADER_LEN..].copy_from_slice(payload);
    let checksum = frame_crc32(frame);
    frame[CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4].copy_from_slice(&checksum.to_le_bytes());
    Ok(frame_len)
}

/// Strictly decodes one complete UDP datagram.
pub fn decode_frame(datagram: &[u8]) -> Result<Frame<'_>, ProtocolError> {
    if datagram.len() < HEADER_LEN {
        return Err(ProtocolError::HeaderTooShort {
            actual: datagram.len(),
        });
    }
    if datagram[..4] != MAGIC {
        return Err(ProtocolError::InvalidMagic);
    }
    if datagram[4] != VERSION {
        return Err(ProtocolError::UnsupportedVersion(datagram[4]));
    }

    let payload_len = u16::from_le_bytes([datagram[24], datagram[25]]) as usize;
    if payload_len > MAX_PAYLOAD_LEN {
        return Err(ProtocolError::PayloadTooLarge {
            actual: payload_len,
            maximum: MAX_PAYLOAD_LEN,
        });
    }
    let declared_len = HEADER_LEN + payload_len;
    if declared_len != datagram.len() {
        return Err(ProtocolError::LengthMismatch {
            declared: declared_len,
            actual: datagram.len(),
        });
    }

    let received = u32::from_le_bytes([
        datagram[CHECKSUM_OFFSET],
        datagram[CHECKSUM_OFFSET + 1],
        datagram[CHECKSUM_OFFSET + 2],
        datagram[CHECKSUM_OFFSET + 3],
    ]);
    let expected = frame_crc32(datagram);
    if expected != received {
        return Err(ProtocolError::ChecksumMismatch { expected, received });
    }

    let message_type = MessageType::try_from(datagram[5])?;
    let error = ErrorCode::try_from(u16::from_le_bytes([datagram[26], datagram[27]]))?;
    let header = Header {
        message_type,
        flags: FrameFlags::from_bits(u16::from_le_bytes([datagram[6], datagram[7]]))?,
        session_id: u32::from_le_bytes([datagram[8], datagram[9], datagram[10], datagram[11]]),
        sequence: u32::from_le_bytes([datagram[12], datagram[13], datagram[14], datagram[15]]),
        timestamp_us: u64::from_le_bytes([
            datagram[16],
            datagram[17],
            datagram[18],
            datagram[19],
            datagram[20],
            datagram[21],
            datagram[22],
            datagram[23],
        ]),
        error,
    };
    validate_header_error(header)?;
    Ok(Frame {
        header,
        payload: &datagram[HEADER_LEN..],
    })
}

fn validate_header_error(header: Header) -> Result<(), ProtocolError> {
    match (header.message_type, header.error) {
        (MessageType::Error, ErrorCode::None) => Err(ProtocolError::MissingErrorCode),
        (MessageType::Error, _) | (_, ErrorCode::None) => Ok(()),
        (_, error) => Err(ProtocolError::UnexpectedErrorCode(error)),
    }
}

/// IEEE CRC-32 with the checksum bytes logically cleared to zero.
fn frame_crc32(frame: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for (index, byte) in frame.iter().copied().enumerate() {
        let value = if (CHECKSUM_OFFSET..CHECKSUM_OFFSET + 4).contains(&index) {
            0
        } else {
            byte
        };
        crc ^= u32::from(value);
        for _ in 0..8 {
            let mask = 0u32.wrapping_sub(crc & 1);
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_header() -> Header {
        Header {
            message_type: MessageType::Control,
            flags: FrameFlags::ACK_REQUIRED,
            session_id: 0x0102_0304,
            sequence: 0x0506_0708,
            timestamp_us: 0x1112_1314_1516_1718,
            error: ErrorCode::None,
        }
    }

    #[test]
    fn golden_control_frame_is_stable() {
        let mut output = [0u8; 64];
        let len = encode_frame(sample_header(), &[0xaa, 0x55], &mut output).unwrap();
        assert_eq!(len, 34);
        assert_eq!(
            &output[..len],
            &[
                0x49, 0x56, 0x43, 0x31, 0x01, 0x01, 0x01, 0x00, 0x04, 0x03, 0x02, 0x01, 0x08, 0x07,
                0x06, 0x05, 0x18, 0x17, 0x16, 0x15, 0x14, 0x13, 0x12, 0x11, 0x02, 0x00, 0x00, 0x00,
                0xea, 0x5d, 0x15, 0xfe, 0xaa, 0x55,
            ]
        );
    }

    #[test]
    fn round_trip_preserves_header_and_payload() {
        let mut output = [0u8; 64];
        let len = encode_frame(sample_header(), b"control", &mut output).unwrap();
        let frame = decode_frame(&output[..len]).unwrap();
        assert_eq!(frame.header, sample_header());
        assert_eq!(frame.payload, b"control");
    }

    #[test]
    fn corrupted_payload_is_rejected() {
        let mut output = [0u8; 64];
        let len = encode_frame(sample_header(), b"control", &mut output).unwrap();
        output[len - 1] ^= 0x80;
        assert!(matches!(
            decode_frame(&output[..len]),
            Err(ProtocolError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn trailing_bytes_are_rejected() {
        let mut output = [0u8; 64];
        let len = encode_frame(sample_header(), b"x", &mut output).unwrap();
        assert_eq!(
            decode_frame(&output[..len + 1]),
            Err(ProtocolError::LengthMismatch {
                declared: len,
                actual: len + 1,
            })
        );
    }
}
