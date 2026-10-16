use super::{FrameKind, FrameSpec};

/// Incremental Message V2 CRC32C over the 37-byte descriptor and complete body.
///
/// Uses the Castagnoli polynomial and initial/final all-ones XOR specified in
/// RFC 4960 Appendix B. This detects corruption, not malicious modification.
/// No body bytes are allocated, retained or copied into a message-sized buffer.
#[derive(Clone, Copy, Debug)]
pub struct MessageChecksum {
    remainder: u32,
}

impl MessageChecksum {
    /// Starts a checksum from the DATA or CONTROL message's fixed metadata.
    ///
    /// `source_peer` must come from the publisher section; `destination_peer`
    /// is the slot destination. This computes the descriptor, not authorization
    /// or frame validation. `spec.message_crc32c` and DATA FIRST/LAST flags are
    /// excluded. All multibyte descriptor fields are explicitly little-endian.
    ///
    /// Returns `None` for ABORT, which retains the cancelled message's original
    /// expected checksum rather than computing a checksum of its empty fragment.
    pub fn new(spec: FrameSpec, source_peer: u16, destination_peer: u16) -> Option<Self> {
        let class = match spec.kind {
            FrameKind::Data { .. } => 0,
            FrameKind::Control => 1,
            FrameKind::Abort => return None,
        };
        let mut checksum = Self {
            remainder: u32::MAX,
        };
        checksum.update(&[class]);
        checksum.update(&source_peer.to_le_bytes());
        checksum.update(&destination_peer.to_le_bytes());
        checksum.update(&spec.source_session.to_le_bytes());
        checksum.update(&spec.destination_session.to_le_bytes());
        checksum.update(&spec.message_id.to_le_bytes());
        checksum.update(&spec.message_len.to_le_bytes());
        Some(checksum)
    }

    /// Feeds the next body bytes in order. Fragment boundaries are irrelevant.
    ///
    /// The message state owns length/offset validation. Feed each body byte
    /// exactly once. For a publication attempt that may return Full, update a
    /// copy and commit it only after the slot is published, not on each retry.
    pub fn update(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.remainder ^= u32::from(byte);
            for _ in 0..8 {
                self.remainder =
                    (self.remainder >> 1) ^ (0x82f6_3b78 & 0u32.wrapping_sub(self.remainder & 1));
            }
        }
    }

    /// Returns the final checksum without modifying the expected CRC in a spec.
    ///
    /// The sender stores this value in its fixed message metadata. The receiver
    /// must validate total length and compare this result with the reserved
    /// message's expected CRC before delivering the body. This accumulator alone
    /// does not establish completeness, valid UTF-8 or matching session state.
    pub const fn finish(self) -> u32 {
        self.remainder ^ u32::MAX
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(kind: FrameKind, message_len: u64) -> FrameSpec {
        FrameSpec {
            kind,
            source_session: 0x0c0b_0a09_0807_0605,
            destination_session: 0x1413_1211_100f_0e0d,
            message_id: 0x1c1b_1a19_1817_1615,
            message_len,
            message_crc32c: 0,
        }
    }

    #[test]
    fn message_checksum_covers_the_wire_descriptor_and_streamed_body() {
        // The conventional CRC32C check string, before adding any descriptor.
        let mut standard = MessageChecksum {
            remainder: u32::MAX,
        };
        standard.update(b"123456789");
        assert_eq!(standard.finish(), 0xe306_9283);

        // Independently computed MSB-first CRC32C reference fixtures. The
        // descriptor is class, bytes 01..1c, then the LE u64 body length.
        let data = FrameKind::Data {
            first: true,
            last: true,
        };
        for (kind, body, expected) in [
            (data, &b""[..], 0x1f8c_ed76),
            (data, &b"abc"[..], 0x79d1_b00e),
            (FrameKind::Control, &b"abc"[..], 0xa411_a525),
        ] {
            let frame_spec = spec(kind, body.len() as u64);
            for split in 0..=body.len() {
                let mut checksum = MessageChecksum::new(frame_spec, 0x0201, 0x0403).unwrap();
                checksum.update(&body[..split]);
                checksum.update(&[]);
                checksum.update(&body[split..]);
                assert_eq!(checksum.finish(), expected, "split={split}");
            }
        }

        let original = spec(data, 3);
        let mut continuation = original;
        continuation.kind = FrameKind::Data {
            first: false,
            last: false,
        };
        continuation.message_crc32c = 0x79d1_b00e;
        let mut checksum = MessageChecksum::new(continuation, 0x0201, 0x0403).unwrap();
        checksum.update(b"abc");
        assert_eq!(checksum.finish(), 0x79d1_b00e);

        let mut abort = original;
        abort.kind = FrameKind::Abort;
        assert!(MessageChecksum::new(abort, 0x0201, 0x0403).is_none());
    }

    #[test]
    fn identity_or_body_corruption_changes_the_expected_checksum() {
        let original = spec(
            FrameKind::Data {
                first: true,
                last: true,
            },
            3,
        );
        let expected = 0x79d1_b00e;
        let mut changed_source_session = original;
        changed_source_session.source_session ^= 1;
        let mut changed_destination_session = original;
        changed_destination_session.destination_session ^= 1;
        let mut changed_id = original;
        changed_id.message_id ^= 1;
        let mut changed_length = original;
        changed_length.message_len ^= 1;
        for (frame_spec, source, destination, body) in [
            (changed_source_session, 0x0201, 0x0403, b"abc"),
            (changed_destination_session, 0x0201, 0x0403, b"abc"),
            (changed_id, 0x0201, 0x0403, b"abc"),
            (changed_length, 0x0201, 0x0403, b"abc"),
            (original, 0x0200, 0x0403, b"abc"),
            (original, 0x0201, 0x0402, b"abc"),
            (original, 0x0201, 0x0403, b"abb"),
        ] {
            let mut checksum = MessageChecksum::new(frame_spec, source, destination).unwrap();
            checksum.update(body);
            assert_ne!(checksum.finish(), expected);
        }
    }
}
