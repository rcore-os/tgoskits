//! `virtio_net_hdr` wire format.
//!
//! The first version does not negotiate mergeable buffers, so the header is the
//! base 10-byte layout. All multi-byte fields are little-endian. Layout and size
//! are asserted at compile time rather than relying on Rust's default struct
//! layout.

use crate::constants::{VIRTIO_NET_HDR_GSO_NONE, VIRTIO_NET_HDR_SIZE};

/// Base `virtio_net_hdr` (no mergeable buffers).
///
/// ```text
/// offset  field
/// 0       u8  flags
/// 1       u8  gso_type
/// 2       le16 hdr_len
/// 4       le16 gso_size
/// 6       le16 csum_start
/// 8       le16 csum_offset
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VirtioNetHdr {
    pub flags: u8,
    pub gso_type: u8,
    pub hdr_len: u16,
    pub gso_size: u16,
    pub csum_start: u16,
    pub csum_offset: u16,
}

impl VirtioNetHdr {
    /// Number of bytes this header occupies on the wire.
    pub const SIZE: usize = VIRTIO_NET_HDR_SIZE;

    /// Decode a header from exactly [`SIZE`](Self::SIZE) little-endian bytes.
    pub fn from_le_bytes(bytes: &[u8]) -> Option<Self> {
        let b: &[u8; Self::SIZE] = bytes.get(..Self::SIZE)?.try_into().ok()?;
        Some(Self {
            flags: b[0],
            gso_type: b[1],
            hdr_len: u16::from_le_bytes([b[2], b[3]]),
            gso_size: u16::from_le_bytes([b[4], b[5]]),
            csum_start: u16::from_le_bytes([b[6], b[7]]),
            csum_offset: u16::from_le_bytes([b[8], b[9]]),
        })
    }

    /// Encode this header to [`SIZE`](Self::SIZE) little-endian bytes.
    pub fn to_le_bytes(&self) -> [u8; Self::SIZE] {
        let mut b = [0u8; Self::SIZE];
        b[0] = self.flags;
        b[1] = self.gso_type;
        b[2..4].copy_from_slice(&self.hdr_len.to_le_bytes());
        b[4..6].copy_from_slice(&self.gso_size.to_le_bytes());
        b[6..8].copy_from_slice(&self.csum_start.to_le_bytes());
        b[8..10].copy_from_slice(&self.csum_offset.to_le_bytes());
        b
    }

    /// Whether the header requests any checksum/GSO offload.
    ///
    /// The first version negotiates no offload features, so any non-zero
    /// offload-related field must be rejected rather than silently sending a
    /// possibly-corrupted frame.
    pub fn requests_offload(&self) -> bool {
        self.flags != 0
            || self.gso_type != VIRTIO_NET_HDR_GSO_NONE
            || self.hdr_len != 0
            || self.gso_size != 0
            || self.csum_start != 0
            || self.csum_offset != 0
    }
}

// The wire format is handled with explicit little-endian byte encoding, so the
// header constant (not the Rust struct layout) is the source of truth.
const _: () = assert!(VIRTIO_NET_HDR_SIZE == 10);
