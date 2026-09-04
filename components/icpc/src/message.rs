//! Message types and frame encode/decode.

use crate::header::{
    HEADER_LEN, Header, PROTOCOL_VERSION, ProtocolError, compute_frame_crc, verify_frame_crc,
};

/// icpc message type codes.
#[repr(u8)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    CtrlCmd     = 0x01,
    StateReport = 0x02,
    ErrorNotify = 0x03,
    Ack         = 0x04,
    Heartbeat   = 0x05,
}

impl TryFrom<u8> for MessageType {
    type Error = ProtocolError;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0x01 => Ok(Self::CtrlCmd),
            0x02 => Ok(Self::StateReport),
            0x03 => Ok(Self::ErrorNotify),
            0x04 => Ok(Self::Ack),
            0x05 => Ok(Self::Heartbeat),
            other => Err(ProtocolError::UnknownMessageType(other)),
        }
    }
}

/// Owned view of a decoded frame (payload borrowed from input buffer).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Message<'a> {
    pub header: Header,
    pub payload: &'a [u8],
}

/// Encodes one frame into `out`. Returns total bytes written.
pub fn encode(
    msg_type: MessageType,
    flags: u8,
    seq: u32,
    timestamp_ns: u64,
    err_code: u16,
    payload: &[u8],
    out: &mut [u8],
) -> Result<usize, ProtocolError> {
    let payload_len = u16::try_from(payload.len()).map_err(|_| ProtocolError::BufferTooSmall)?;
    let total = HEADER_LEN + payload.len();
    if out.len() < total {
        return Err(ProtocolError::BufferTooSmall);
    }

    let header = Header {
        version: PROTOCOL_VERSION,
        msg_type,
        flags,
        seq,
        timestamp_ns,
        payload_len,
        err_code,
        crc32: 0,
    };
    header.write_to(out)?;
    out[HEADER_LEN..total].copy_from_slice(payload);
    let crc = compute_frame_crc(&out[..HEADER_LEN], payload);
    out[20..24].copy_from_slice(&crc.to_le_bytes());
    Ok(total)
}

/// Decodes one frame from `frame`, verifying version, length, and CRC.
pub fn decode(frame: &[u8]) -> Result<Message<'_>, ProtocolError> {
    let header = Header::parse(frame)?;
    let payload_len = header.payload_len as usize;
    if frame.len() < HEADER_LEN + payload_len {
        return Err(ProtocolError::TruncatedPayload);
    }
    verify_frame_crc(frame, payload_len)?;
    Ok(Message {
        header,
        payload: &frame[HEADER_LEN..HEADER_LEN + payload_len],
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_ctrl_cmd() {
        let mut buf = [0u8; 64];
        let n = encode(
            MessageType::CtrlCmd,
            0,
            7,
            1_000_000,
            0,
            b"kp=1.2",
            &mut buf,
        )
        .unwrap();
        let msg = decode(&buf[..n]).unwrap();
        assert_eq!(msg.header.msg_type, MessageType::CtrlCmd);
        assert_eq!(msg.header.seq, 7);
        assert_eq!(msg.payload, b"kp=1.2");
    }

    #[test]
    fn rejects_bad_crc() {
        let mut buf = [0u8; 64];
        let n = encode(MessageType::Heartbeat, 0, 1, 0, 0, b"", &mut buf).unwrap();
        buf[20] ^= 0xff;
        assert_eq!(decode(&buf[..n]), Err(ProtocolError::BadChecksum));
    }

    #[test]
    fn three_business_types_encode() {
        let mut buf = [0u8; 128];
        for ty in [
            MessageType::CtrlCmd,
            MessageType::StateReport,
            MessageType::ErrorNotify,
        ] {
            let n = encode(ty, 0, 1, 0, 0, b"x", &mut buf).unwrap();
            let msg = decode(&buf[..n]).unwrap();
            assert_eq!(msg.header.msg_type, ty);
        }
    }
}
