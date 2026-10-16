use thiserror::Error;

use super::{FRAGMENT_SIZE, HEADER_SIZE};

const MESSAGE_VERSION: u8 = 2;
const FLAG_FIRST: u8 = 1;
const FLAG_LAST: u8 = 2;
const FLAG_ABORT: u8 = 4;
const FLAG_CONTROL: u8 = 8;
const KNOWN_FLAGS: u8 = FLAG_FIRST | FLAG_LAST | FLAG_ABORT | FLAG_CONTROL;

/// The legal flag combinations of a DATA, ABORT or single-frame CONTROL.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameKind {
    /// One fragment of an application message, including an empty message.
    Data { first: bool, last: bool },
    /// Cancels a message using its original identity, total length and CRC.
    Abort,
    /// A complete binary control message; always FIRST | LAST | CONTROL.
    Control,
}

/// Message metadata repeated in every fragment. Integers are wire values,
/// not Rust struct layout. Encoding validates frame-local constraints only.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameSpec {
    pub kind: FrameKind,
    /// Nonzero DATA or CONTROL identifier assigned by the transport service.
    pub message_id: u64,
    /// Complete body length in bytes, not the length of this fragment.
    pub message_len: u64,
    pub source_session: u64,
    pub destination_session: u64,
    /// Expected CRC32C of the message descriptor and complete body.
    pub message_crc32c: u32,
}

impl FrameSpec {
    /// Encodes a header for `fragment_len` bytes without copying the fragment.
    ///
    /// The returned header and original fragment can be published separately.
    /// This does not access shared memory or validate the message checksum.
    /// An error returns no encoded header and changes no external state.
    pub fn encode_header(self, fragment_len: usize) -> Result<[u8; HEADER_SIZE], FrameError> {
        validate_frame(&self, fragment_len)?;
        let mut header = [0u8; HEADER_SIZE];
        header[0] = MESSAGE_VERSION;
        header[1] = match self.kind {
            FrameKind::Data { first, last } => {
                (if first { FLAG_FIRST } else { 0 }) | (if last { FLAG_LAST } else { 0 })
            }
            FrameKind::Abort => FLAG_ABORT,
            FrameKind::Control => FLAG_FIRST | FLAG_LAST | FLAG_CONTROL,
        };
        header[2..4].copy_from_slice(&(HEADER_SIZE as u16).to_le_bytes());
        header[4..8].copy_from_slice(&(fragment_len as u32).to_le_bytes());
        header[8..16].copy_from_slice(&self.message_id.to_le_bytes());
        header[16..24].copy_from_slice(&self.message_len.to_le_bytes());
        header[24..32].copy_from_slice(&self.source_session.to_le_bytes());
        header[32..40].copy_from_slice(&self.destination_session.to_le_bytes());
        header[40..44].copy_from_slice(&self.message_crc32c.to_le_bytes());
        Ok(header)
    }
}

/// One structurally valid frame borrowing an ordinary, local slot copy.
/// This is not proof of a valid session, reservation or complete message.
#[derive(Debug, Eq, PartialEq)]
pub struct DecodedFrame<'a> {
    pub spec: FrameSpec,
    pub fragment: &'a [u8],
}

impl<'a> DecodedFrame<'a> {
    /// Decodes exactly the slot's used payload bytes, with no trailing padding.
    ///
    /// Never casts shared memory to a struct or allocates from untrusted
    /// lengths. The caller must first copy shared atomic words into local
    /// storage using the ring's access contract.
    pub fn decode(payload: &'a [u8]) -> Result<Self, FrameError> {
        if payload.len() < HEADER_SIZE {
            return Err(FrameError::InvalidHeader);
        }
        if payload[0] != MESSAGE_VERSION {
            return Err(FrameError::UnsupportedVersion {
                version: payload[0],
            });
        }
        let flags = payload[1];
        if flags & !KNOWN_FLAGS != 0 {
            return Err(FrameError::UnknownFlags { flags });
        }
        if u16::from_le_bytes([payload[2], payload[3]]) as usize != HEADER_SIZE {
            return Err(FrameError::InvalidHeader);
        }
        let fragment_len =
            usize::try_from(read_u32(payload, 4)).map_err(|_| FrameError::InvalidFragmentLength)?;
        if fragment_len != payload.len() - HEADER_SIZE {
            return Err(FrameError::InvalidFragmentLength);
        }
        let kind = if flags & FLAG_CONTROL != 0 {
            if flags != FLAG_FIRST | FLAG_LAST | FLAG_CONTROL {
                return Err(FrameError::InvalidHeader);
            }
            FrameKind::Control
        } else if flags & FLAG_ABORT != 0 {
            if flags != FLAG_ABORT {
                return Err(FrameError::InvalidHeader);
            }
            FrameKind::Abort
        } else {
            FrameKind::Data {
                first: flags & FLAG_FIRST != 0,
                last: flags & FLAG_LAST != 0,
            }
        };
        let spec = FrameSpec {
            kind,
            message_id: read_u64(payload, 8),
            message_len: read_u64(payload, 16),
            source_session: read_u64(payload, 24),
            destination_session: read_u64(payload, 32),
            message_crc32c: read_u32(payload, 40),
        };
        validate_frame(&spec, fragment_len)?;
        Ok(Self {
            spec,
            fragment: &payload[HEADER_SIZE..],
        })
    }
}

/// Frame-local decoding or encoding failure, before connection state changes.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum FrameError {
    #[error("unsupported message version {version}")]
    UnsupportedVersion { version: u8 },
    #[error("unknown message flags {flags:#04x}")]
    UnknownFlags { flags: u8 },
    #[error("invalid message header or flag combination")]
    InvalidHeader,
    #[error("invalid fragment length")]
    InvalidFragmentLength,
    #[error("message identifier must be nonzero")]
    ZeroMessageId,
    #[error("DATA and ABORT require nonzero source and destination sessions")]
    InvalidSession,
}

fn validate_frame(spec: &FrameSpec, fragment_len: usize) -> Result<(), FrameError> {
    if spec.message_id == 0 {
        return Err(FrameError::ZeroMessageId);
    }
    if fragment_len > FRAGMENT_SIZE {
        return Err(FrameError::InvalidFragmentLength);
    }
    // CONTROL may use session 0. Which opcode may do so depends on the
    // connection phase and is deliberately not decided by the frame codec.
    if spec.kind != FrameKind::Control
        && (spec.source_session == 0 || spec.destination_session == 0)
    {
        return Err(FrameError::InvalidSession);
    }
    match spec.kind {
        FrameKind::Abort => {
            if fragment_len != 0 {
                return Err(FrameError::InvalidFragmentLength);
            }
        }
        FrameKind::Control => {
            // The first body byte is the opcode, so CONTROL cannot be empty.
            if fragment_len == 0 || fragment_len as u64 != spec.message_len {
                return Err(FrameError::InvalidFragmentLength);
            }
        }
        FrameKind::Data { first, last } => {
            let len = fragment_len as u64;
            if spec.message_len == 0 {
                if !first || !last || len != 0 {
                    return Err(FrameError::InvalidHeader);
                }
            } else if len == 0
                || len > spec.message_len
                || (!last && len == spec.message_len)
                || (first && last && len != spec.message_len)
            {
                return Err(FrameError::InvalidFragmentLength);
            }
        }
    }
    Ok(())
}

fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    // Independent wire fixture: all multibyte fields have distinguishable
    // bytes so a symmetric encoder/decoder offset mistake cannot pass.
    const WIRE: [u8; 47] = [
        2, 3, 44, 0, 3, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 3, 0, 0, 0, 0, 0, 0, 0, 17, 18, 19, 20,
        21, 22, 23, 24, 33, 34, 35, 36, 37, 38, 39, 40, 49, 50, 51, 52, b'a', b'b', b'c',
    ];

    fn spec() -> FrameSpec {
        FrameSpec {
            kind: FrameKind::Data {
                first: true,
                last: true,
            },
            message_id: 0x0807_0605_0403_0201,
            message_len: 3,
            source_session: 0x1817_1615_1413_1211,
            destination_session: 0x2827_2625_2423_2221,
            message_crc32c: 0x3433_3231,
        }
    }

    #[test]
    fn codec_preserves_v2_wire_layout_and_fragment_boundaries() {
        assert_eq!(spec().encode_header(3).unwrap(), WIRE[..HEADER_SIZE]);
        let decoded = DecodedFrame::decode(&WIRE).unwrap();
        assert_eq!(decoded.spec, spec());
        assert_eq!(decoded.fragment, b"abc");

        for (kind, total_len, fragment_len, flags) in [
            (
                FrameKind::Data {
                    first: true,
                    last: true,
                },
                0,
                0,
                3,
            ),
            (
                FrameKind::Data {
                    first: true,
                    last: false,
                },
                500,
                FRAGMENT_SIZE,
                1,
            ),
            (
                FrameKind::Data {
                    first: false,
                    last: false,
                },
                500,
                100,
                0,
            ),
            (
                FrameKind::Data {
                    first: false,
                    last: true,
                },
                500,
                100,
                2,
            ),
            (FrameKind::Abort, 500, 0, 4),
            (FrameKind::Control, 1, 1, 11),
        ] {
            let mut frame_spec = FrameSpec {
                kind,
                message_len: total_len,
                ..spec()
            };
            if kind == FrameKind::Control {
                frame_spec.destination_session = 0;
            }
            let header = frame_spec.encode_header(fragment_len).unwrap();
            assert_eq!(header[1], flags);
            let mut payload = [0u8; crate::PAYLOAD_SIZE];
            payload[..HEADER_SIZE].copy_from_slice(&header);
            let decoded = DecodedFrame::decode(&payload[..HEADER_SIZE + fragment_len]).unwrap();
            assert_eq!(decoded.spec, frame_spec);
            assert_eq!(decoded.fragment.len(), fragment_len);
        }
    }

    #[test]
    fn codec_rejects_incompatible_or_malformed_frames() {
        for len in 0..HEADER_SIZE {
            assert_eq!(
                DecodedFrame::decode(&WIRE[..len]),
                Err(FrameError::InvalidHeader)
            );
        }
        for (offset, value, error) in [
            (0, 1, FrameError::UnsupportedVersion { version: 1 }),
            (1, 0x80, FrameError::UnknownFlags { flags: 0x80 }),
            (1, FLAG_CONTROL, FrameError::InvalidHeader),
            (1, 15, FrameError::InvalidHeader),
            (1, FLAG_ABORT | FLAG_FIRST, FrameError::InvalidHeader),
            (1, FLAG_ABORT, FrameError::InvalidFragmentLength),
            (2, 43, FrameError::InvalidHeader),
            (4, 197, FrameError::InvalidFragmentLength),
            (16, 2, FrameError::InvalidFragmentLength),
            (16, 4, FrameError::InvalidFragmentLength),
        ] {
            let mut payload = WIRE;
            payload[offset] = value;
            assert_eq!(DecodedFrame::decode(&payload), Err(error));
        }
        for (range, error) in [
            (8..16, FrameError::ZeroMessageId),
            (24..32, FrameError::InvalidSession),
            (32..40, FrameError::InvalidSession),
        ] {
            let mut payload = WIRE;
            payload[range].fill(0);
            assert_eq!(DecodedFrame::decode(&payload), Err(error));
        }
        let mut oversized = [0; crate::PAYLOAD_SIZE + 1];
        oversized[..HEADER_SIZE].copy_from_slice(&WIRE[..HEADER_SIZE]);
        oversized[4..8].copy_from_slice(&197u32.to_le_bytes());
        assert_eq!(
            DecodedFrame::decode(&oversized),
            Err(FrameError::InvalidFragmentLength)
        );
        assert_eq!(
            DecodedFrame::decode(&WIRE[..46]),
            Err(FrameError::InvalidFragmentLength)
        );
        assert_eq!(
            spec().encode_header(FRAGMENT_SIZE + 1),
            Err(FrameError::InvalidFragmentLength)
        );
        assert_eq!(
            spec().encode_header(0),
            Err(FrameError::InvalidFragmentLength)
        );
    }
}
