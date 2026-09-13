//! VirtIO-net specific constants.
//!
//! Standard VirtIO transport constants (status bits, MMIO offsets, interrupt
//! bits, `VIRTIO_F_VERSION_1`) come from `axvirtio_common::constants`; this
//! module only defines network-specific values.

// --- Feature bits (first version advertises MAC, STATUS and VERSION_1 only) ---

/// Device feature: MAC address provided in config space.
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;
/// Device feature: link status reported in config space.
pub const VIRTIO_NET_F_STATUS: u64 = 1 << 16;
/// Device feature: maximum MTU reported in config space (not in first version).
pub const VIRTIO_NET_F_MTU: u64 = 1 << 3;
/// Features advertised by the first version: only what is fully implemented.
pub const AXVIRTIO_NET_FEATURES: u64 =
    axvirtio_common::VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC | VIRTIO_NET_F_STATUS;

// --- Queue indices ---

/// Receive queue: the device writes RX frames into guest buffers submitted here.
pub const RX_QUEUE_INDEX: u16 = 0;
/// Transmit queue: the device reads guest TX frames from here.
pub const TX_QUEUE_INDEX: u16 = 1;
/// Number of queue pairs supported by the first version (1 RX + 1 TX).
pub const NUM_QUEUES: u16 = 2;

// --- Link status (config space `status` field) ---

/// Link is up.
pub const VIRTIO_NET_S_LINK_UP: u16 = 1;
/// Driver needs to process announce (not used in first version).
pub const VIRTIO_NET_S_ANNOUNCE: u16 = 2;

// --- virtio_net_hdr ---

/// `gso_type` value meaning "no GSO".
pub const VIRTIO_NET_HDR_GSO_NONE: u8 = 0;

/// Size of the legacy `virtio_net_hdr` without the trailing `num_buffers` field.
pub const VIRTIO_NET_HDR_SIZE: usize = 10;

/// Header size used by the workspace's modern `virtio-drivers` guest driver.
///
/// `virtio-drivers` 0.13.0 includes the trailing `num_buffers` field whenever
/// `VIRTIO_F_VERSION_1` is negotiated, even without `VIRTIO_NET_F_MRG_RXBUF`.
/// The device follows that guest-facing ABI until the driver is corrected.
pub const VIRTIO_NET_HDR_MODERN_SIZE: usize = 12;

// --- Config space layout (relative to VIRTIO_MMIO_CONFIG_OFFSET = 0x100) ---

/// Offset of the MAC address (6 bytes).
pub const VIRTIO_NET_CFG_MAC: u64 = 0x00;
/// Offset of the link status (u16).
pub const VIRTIO_NET_CFG_STATUS: u64 = 0x06;
/// Offset of `max_virtqueue_pairs` (u16), only valid with MQ (not supported).
pub const VIRTIO_NET_CFG_MAX_VQ_PAIRS: u64 = 0x08;
/// Offset of the MTU (u16), only valid with `VIRTIO_NET_F_MTU`.
pub const VIRTIO_NET_CFG_MTU: u64 = 0x0a;

/// Default MTU when the feature is not negotiated.
pub const DEFAULT_MTU: u16 = 1500;
/// Maximum L2 frame size accepted on RX (excludes FCS).
pub const MAX_FRAME_SIZE: usize = 65535;
