//! VirtIO-net device configuration: MAC, link status, optional MTU.

/// Link state. A bare `bool` is intentionally avoided (plan section 7.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkStatus {
    /// Link is down; no RX/TX is possible.
    Down,
    /// Link is up.
    Up,
}

impl LinkStatus {
    /// Wire value written to the config-space `status` field.
    pub fn status_bits(self) -> u16 {
        match self {
            Self::Down => 0,
            Self::Up => crate::constants::VIRTIO_NET_S_LINK_UP,
        }
    }
}

/// Static device configuration supplied at construction time.
#[derive(Debug, Clone)]
pub struct VirtioNetConfig {
    /// MAC address (6 bytes). Exposed via `VIRTIO_NET_F_MAC`.
    pub mac: [u8; 6],
    /// Initial link status. Exposed via `VIRTIO_NET_F_STATUS`.
    pub link: LinkStatus,
    /// MTU. Only meaningful when `VIRTIO_NET_F_MTU` is negotiated, which the
    /// first version does not advertise; stored for future use.
    pub mtu: Option<u16>,
}

impl VirtioNetConfig {
    /// Build a config with the given MAC and link up.
    pub fn new(mac: [u8; 6]) -> Self {
        Self {
            mac,
            link: LinkStatus::Up,
            mtu: None,
        }
    }
}

impl Default for VirtioNetConfig {
    fn default() -> Self {
        // QEMU-style default MAC 52:54:00:... so a guest without explicit config
        // still gets a unicast, locally-administered address.
        Self {
            mac: [0x52, 0x54, 0x00, 0x12, 0x34, 0x56],
            link: LinkStatus::Up,
            mtu: None,
        }
    }
}
