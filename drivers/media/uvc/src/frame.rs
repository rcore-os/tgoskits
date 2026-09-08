use bitflags::bitflags;

bitflags! {
    /// 载荷头标志 (2.4.3.3)。
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(crate) struct PayloadHeaderFlags: u8 {
        const EOH = 1 << 7; // End of Header
        const ERR = 1 << 6; // Error
        const STI = 1 << 5; // Still Image
        const RES = 1 << 4; // Reserved
        const SCR = 1 << 3; // Source Clock Reference
        const PTS = 1 << 2; // Presentation Time Stamp
        const EOF = 1 << 1; // End of Frame
        const FID = 1 << 0; // Frame ID
    }
}

#[derive(Debug, Clone)]
pub(crate) struct UvcPayloadHeader {
    pub flag: PayloadHeaderFlags,
    #[allow(dead_code)]
    pub pts: Option<u32>,
}

impl Default for UvcPayloadHeader {
    fn default() -> Self {
        Self {
            flag: PayloadHeaderFlags::empty(),
            pts: None,
        }
    }
}

impl UvcPayloadHeader {
    /// Parse payload header.
    pub(crate) fn parse(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < 2 {
            return None;
        }
        let b_length = buf[0] as usize;
        let flag = PayloadHeaderFlags::from_bits_truncate(buf[1]);
        if b_length < 2 || b_length > buf.len() {
            return None;
        }

        let has_pts = flag.contains(PayloadHeaderFlags::PTS);
        let has_scr = flag.contains(PayloadHeaderFlags::SCR);

        let mut offset = 2usize;
        let pts = if has_pts {
            if offset + 4 > b_length {
                return None;
            }
            let v = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]);
            offset += 4;
            Some(v)
        } else {
            None
        };

        if has_scr && offset + 6 > b_length {
            return None;
        }

        let header = UvcPayloadHeader { flag, pts };

        Some((header, b_length))
    }
}

/// Frame parser.
#[derive(Debug, Default)]
pub(crate) struct FrameParser {
    last_fid: Option<bool>,
    filled: usize,
    synced: bool,
}

/// 单次 `push_packet` 的结果——以枚举使非法组合不可表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushOutcome {
    Pending,
    Completed { bytes: usize },
    CompletedAndRetry { bytes: usize },
}

impl FrameParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Process one packet.
    pub(crate) fn push_packet(&mut self, data: &[u8], dest: &mut [u8]) -> PushOutcome {
        let (hdr, hdr_len) = match UvcPayloadHeader::parse(data) {
            Some(v) => v,
            None => return PushOutcome::Pending,
        };

        let fid = hdr.flag.contains(PayloadHeaderFlags::FID);
        let eof = hdr.flag.contains(PayloadHeaderFlags::EOF);
        let fid_toggle = self.last_fid.is_some_and(|last| last != fid);
        if fid_toggle && self.filled > 0 {
            let bytes = self.finish_frame();
            return PushOutcome::CompletedAndRetry { bytes };
        }

        if !self.synced {
            match self.last_fid {
                None => {
                    self.last_fid = Some(fid);
                    return PushOutcome::Pending;
                }
                Some(last) if last == fid => return PushOutcome::Pending,
                Some(_) => self.synced = true,
            }
        }
        self.last_fid = Some(fid);

        let payload = &data[hdr_len..];
        let room = dest.len() - self.filled;
        let take = payload.len().min(room);
        dest[self.filled..self.filled + take].copy_from_slice(&payload[..take]);
        self.filled += take;

        if eof && self.filled > 0 {
            let bytes = self.finish_frame();
            return PushOutcome::Completed { bytes };
        }

        PushOutcome::Pending
    }

    fn finish_frame(&mut self) -> usize {
        let bytes = self.filled;
        self.filled = 0;
        bytes
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use super::*;

    const FID0: u8 = 0;
    const FID1: u8 = PayloadHeaderFlags::FID.bits();
    const EOF: u8 = PayloadHeaderFlags::EOF.bits();

    fn pkt(flags: u8, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::with_capacity(2 + payload.len());
        v.push(2);
        v.push(flags);
        v.extend_from_slice(payload);
        v
    }

    #[test]
    fn sync_discards_until_first_fid_toggle() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        assert_eq!(
            p.push_packet(&pkt(FID0, b"aaa"), &mut dest),
            PushOutcome::Pending
        );
        assert_eq!(
            p.push_packet(&pkt(FID0, b"bbb"), &mut dest),
            PushOutcome::Pending
        );
        assert_eq!(dest, vec![0u8; 1024]);
        assert_eq!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8c"), &mut dest),
            PushOutcome::Pending
        );
        let r = p.push_packet(&pkt(FID1 | EOF, b"dd\xFF\xD9"), &mut dest);
        assert_eq!(r, PushOutcome::Completed { bytes: 7 });
        assert_eq!(&dest[..7], b"\xFF\xD8cdd\xFF\xD9");
    }

    #[test]
    fn frame_completes_on_eof() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        assert_eq!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8bb"), &mut dest),
            PushOutcome::Pending
        );
        assert_eq!(
            p.push_packet(&pkt(FID1, b"cc"), &mut dest),
            PushOutcome::Pending
        );
        let r = p.push_packet(&pkt(FID1 | EOF, b"dd\xFF\xD9"), &mut dest);
        assert_eq!(r, PushOutcome::Completed { bytes: 10 });
        assert_eq!(&dest[..10], b"\xFF\xD8bbccdd\xFF\xD9");
    }

    #[test]
    fn full_buffer_truncates_and_waits_for_eof() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 8];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        let r = p.push_packet(&pkt(FID1, b"\xFF\xD8123456789"), &mut dest);
        assert_eq!(r, PushOutcome::Pending);
        assert_eq!(dest, b"\xFF\xD8123456");
        let r = p.push_packet(&pkt(FID1 | EOF, b"xy"), &mut dest);
        assert_eq!(r, PushOutcome::Completed { bytes: 8 });
    }

    #[test]
    fn fid_toggle_completes_frame_and_retries_packet() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 64];
        assert_eq!(
            p.push_packet(&pkt(FID0, b"x"), &mut dest),
            PushOutcome::Pending
        );
        assert_eq!(
            p.push_packet(&pkt(FID1, b"\xFF\xD8f1\xFF\xD9"), &mut dest),
            PushOutcome::Pending
        );
        let r = p.push_packet(&pkt(FID0, b"\xFF\xD8f2"), &mut dest);
        assert_eq!(r, PushOutcome::CompletedAndRetry { bytes: 6 });
        assert_eq!(&dest[..6], b"\xFF\xD8f1\xFF\xD9");
        let mut dest2 = vec![0u8; 64];
        let r2 = p.push_packet(&pkt(FID0, b"\xFF\xD8f2"), &mut dest2);
        assert_eq!(r2, PushOutcome::Pending);
        assert_eq!(&dest2[..4], b"\xFF\xD8f2");
        let r3 = p.push_packet(&pkt(FID0 | EOF, b"tail\xFF\xD9"), &mut dest2);
        assert_eq!(r3, PushOutcome::Completed { bytes: 10 });
        assert_eq!(&dest2[..10], b"\xFF\xD8f2tail\xFF\xD9");
    }

    #[test]
    fn invalid_header_drops_packet_without_touching_fid_state() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 1024];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        let bad = vec![2u8, PayloadHeaderFlags::PTS.bits()];
        assert_eq!(p.push_packet(&bad, &mut dest), PushOutcome::Pending);
        let r = p.push_packet(&pkt(FID1 | EOF, b"\xFF\xD8bb\xFF\xD9"), &mut dest);
        assert_eq!(r, PushOutcome::Completed { bytes: 6 });
    }
}
