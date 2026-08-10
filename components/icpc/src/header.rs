//! Fixed 24-byte icpc header.

use core::fmt;

use thiserror::Error;

use crate::message::MessageType;

/// Wire header length in bytes.
pub const HEADER_LEN: usize = 24;

/// Current protocol version.
pub const PROTOCOL_VERSION: u8 = 1;

/// Parsed icpc header fields.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Header {
    pub version: u8,
    pub msg_type: MessageType,
    pub flags: u8,
    pub seq: u32,
    pub timestamp_ns: u64,
    pub payload_len: u16,
    pub err_code: u16,
    pub crc32: u32,
}

/// Protocol encode / decode failures.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ProtocolError {
    #[error("buffer too short for icpc header")]
    TruncatedHeader,
    #[error("buffer too short for declared payload")]
    TruncatedPayload,
    #[error("unsupported protocol version {0}")]
    UnsupportedVersion(u8),
    #[error("unknown message type {0:#x}")]
    UnknownMessageType(u8),
    #[error("crc32 mismatch")]
    BadChecksum,
    #[error("output buffer too small")]
    BufferTooSmall,
}

impl Header {
    /// Encodes the header into `out` (must be at least [`HEADER_LEN`] bytes).
    ///
    /// The CRC field is written as zero first; callers must overwrite it after
    /// hashing header+payload, or use [`crate::encode`].
    pub fn write_to(&self, out: &mut [u8]) -> Result<(), ProtocolError> {
        if out.len() < HEADER_LEN {
            return Err(ProtocolError::BufferTooSmall);
        }
        out[0] = self.version;
        out[1] = self.msg_type as u8;
        out[2] = self.flags;
        out[3] = 0;
        out[4..8].copy_from_slice(&self.seq.to_le_bytes());
        out[8..16].copy_from_slice(&self.timestamp_ns.to_le_bytes());
        out[16..18].copy_from_slice(&self.payload_len.to_le_bytes());
        out[18..20].copy_from_slice(&self.err_code.to_le_bytes());
        out[20..24].copy_from_slice(&0u32.to_le_bytes());
        Ok(())
    }

    /// Parses a header without verifying CRC (CRC is checked against payload).
    pub fn parse(bytes: &[u8]) -> Result<Self, ProtocolError> {
        if bytes.len() < HEADER_LEN {
            return Err(ProtocolError::TruncatedHeader);
        }
        let version = bytes[0];
        if version != PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion(version));
        }
        let msg_type = MessageType::try_from(bytes[1])?;
        let flags = bytes[2];
        let seq = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        let timestamp_ns = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        let payload_len = u16::from_le_bytes(bytes[16..18].try_into().unwrap());
        let err_code = u16::from_le_bytes(bytes[18..20].try_into().unwrap());
        let crc = u32::from_le_bytes(bytes[20..24].try_into().unwrap());
        Ok(Self {
            version,
            msg_type,
            flags,
            seq,
            timestamp_ns,
            payload_len,
            err_code,
            crc32: crc,
        })
    }
}

impl fmt::Display for Header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "icpc v{} {:?} seq={} len={} err={}",
            self.version, self.msg_type, self.seq, self.payload_len, self.err_code
        )
    }
}

/// Computes CRC covering header (CRC field zeroed) + payload.
pub(crate) fn compute_frame_crc(header_bytes: &[u8], payload: &[u8]) -> u32 {
    debug_assert!(header_bytes.len() >= HEADER_LEN);
    let mut crc = 0xffff_ffff_u32;
    // Inline IEEE update over header with zeroed CRC bytes.
    for (i, &b) in header_bytes[..HEADER_LEN].iter().enumerate() {
        let byte = if (20..24).contains(&i) { 0 } else { b };
        crc = crc_update_byte(crc, byte);
    }
    for &b in payload {
        crc = crc_update_byte(crc, b);
    }
    !crc
}

fn crc_update_byte(mut crc: u32, byte: u8) -> u32 {
    const POLY: u32 = 0xEDB8_8320;
    crc ^= u32::from(byte);
    for _ in 0..8 {
        let mask = (crc & 1).wrapping_neg();
        crc = (crc >> 1) ^ (POLY & mask);
    }
    crc
}

/// Verifies that `frame` CRC matches.
pub(crate) fn verify_frame_crc(frame: &[u8], payload_len: usize) -> Result<(), ProtocolError> {
    if frame.len() < HEADER_LEN + payload_len {
        return Err(ProtocolError::TruncatedPayload);
    }
    let expected = u32::from_le_bytes(frame[20..24].try_into().unwrap());
    let actual = compute_frame_crc(
        &frame[..HEADER_LEN],
        &frame[HEADER_LEN..HEADER_LEN + payload_len],
    );
    if expected != actual {
        return Err(ProtocolError::BadChecksum);
    }
    Ok(())
}
