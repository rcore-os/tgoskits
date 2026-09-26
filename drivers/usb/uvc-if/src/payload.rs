//! UVC isochronous payload header wire format.

use bitflags::bitflags;

bitflags! {
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub struct PayloadHeaderFlags: u8 {
        const EOH = 1 << 7;
        const ERR = 1 << 6;
        const STI = 1 << 5;
        const RES = 1 << 4;
        const SCR = 1 << 3;
        const PTS = 1 << 2;
        const EOF = 1 << 1;
        const FID = 1 << 0;
    }
}

#[derive(Debug, Clone, Default)]
pub struct UvcPayloadHeader {
    pub length: u8,
    pub info: u8,
    pub fid: bool,
    pub eof: bool,
    pub pts: Option<u32>,
    pub scr: Option<(u32, u16)>,
    pub has_err: bool,
}

impl UvcPayloadHeader {
    pub fn parse(buf: &[u8]) -> Option<(Self, usize)> {
        let header_len = *buf.first()? as usize;
        let info = *buf.get(1)?;
        if header_len < 2 || header_len > buf.len() {
            return None;
        }
        let header = &buf[..header_len];
        let flags = PayloadHeaderFlags::from_bits_truncate(info);
        let mut offset = 2;
        let pts = if flags.contains(PayloadHeaderFlags::PTS) {
            let value = u32::from_le_bytes(header.get(offset..offset + 4)?.try_into().ok()?);
            offset += 4;
            Some(value)
        } else {
            None
        };
        let scr = if flags.contains(PayloadHeaderFlags::SCR) {
            let stc = u32::from_le_bytes(header.get(offset..offset + 4)?.try_into().ok()?);
            let sof = u16::from_le_bytes(header.get(offset + 4..offset + 6)?.try_into().ok()?);
            Some((stc, sof))
        } else {
            None
        };
        Some((
            Self {
                length: header_len as u8,
                info,
                fid: flags.contains(PayloadHeaderFlags::FID),
                eof: flags.contains(PayloadHeaderFlags::EOF),
                pts,
                scr,
                has_err: flags.contains(PayloadHeaderFlags::ERR),
            },
            header_len,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn optional_fields_stay_within_header_length() {
        let bytes = [2, PayloadHeaderFlags::PTS.bits(), 1, 2, 3, 4];
        assert!(UvcPayloadHeader::parse(&bytes).is_none());
        let bytes = [
            12,
            (PayloadHeaderFlags::PTS | PayloadHeaderFlags::SCR).bits(),
            1,
            2,
            3,
            4,
            5,
            6,
            7,
            8,
            9,
            10,
        ];
        let (header, len) = UvcPayloadHeader::parse(&bytes).unwrap();
        assert_eq!(len, 12);
        assert_eq!(header.pts, Some(0x04030201));
        assert_eq!(header.scr, Some((0x08070605, 0x0a09)));
    }
}
