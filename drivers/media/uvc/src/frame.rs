#[cfg(test)]
use uvc_if::payload::PayloadHeaderFlags;
use uvc_if::payload::UvcPayloadHeader;

/// Frame parser.
#[derive(Debug, Default)]
pub(crate) struct FrameParser {
    last_fid: Option<bool>,
    filled: usize,
    invalid: bool,
    synced: bool,
}

/// 单次 `push_packet` 的结果——以枚举使非法组合不可表示。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PushOutcome {
    Pending,
    Completed { bytes: usize },
    CompletedAndRetry { bytes: usize },
    Discarded,
    DiscardedAndRetry,
}

impl FrameParser {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Process one packet.
    pub(crate) fn push_packet(&mut self, data: &[u8], dest: &mut [u8]) -> PushOutcome {
        let (hdr, hdr_len) = match UvcPayloadHeader::parse(data) {
            Some(v) => v,
            None => {
                if self.synced {
                    self.invalid = true;
                }
                return PushOutcome::Pending;
            }
        };

        let fid = hdr.fid;
        let eof = hdr.eof;
        let fid_toggle = self.last_fid.is_some_and(|last| last != fid);
        if fid_toggle && (self.filled > 0 || self.invalid) {
            let (bytes, invalid) = self.finish_frame();
            return if invalid {
                PushOutcome::DiscardedAndRetry
            } else {
                PushOutcome::CompletedAndRetry { bytes }
            };
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

        if hdr.has_err {
            self.invalid = true;
        }

        let payload = &data[hdr_len..];
        let room = dest.len() - self.filled;
        let take = payload.len().min(room);
        if take != payload.len() {
            self.invalid = true;
        }
        dest[self.filled..self.filled + take].copy_from_slice(&payload[..take]);
        self.filled += take;

        if eof && (self.filled > 0 || self.invalid) {
            let (bytes, invalid) = self.finish_frame();
            return if invalid {
                PushOutcome::Discarded
            } else {
                PushOutcome::Completed { bytes }
            };
        }

        PushOutcome::Pending
    }

    fn finish_frame(&mut self) -> (usize, bool) {
        let bytes = self.filled;
        let invalid = self.invalid;
        self.filled = 0;
        self.invalid = false;
        (bytes, invalid)
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
    fn full_buffer_discards_frame_at_eof() {
        let mut p = FrameParser::new();
        let mut dest = vec![0u8; 8];
        p.push_packet(&pkt(FID0, b"a"), &mut dest);
        let r = p.push_packet(&pkt(FID1, b"\xFF\xD8123456789"), &mut dest);
        assert_eq!(r, PushOutcome::Pending);
        assert_eq!(dest, b"\xFF\xD8123456");
        let r = p.push_packet(&pkt(FID1 | EOF, b"xy"), &mut dest);
        assert!(!matches!(r, PushOutcome::Completed { .. }));
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
