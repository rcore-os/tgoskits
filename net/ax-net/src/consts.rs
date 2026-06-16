//! Shared buffer sizes and queue limits.
//!
//! These constants define ax-net's default memory budget for protocol sockets,
//! smoltcp packet buffers, listen queues, pending ARP packets, and per-device
//! worker queues. They are intentionally centralized so embedded targets can
//! audit memory growth without chasing per-protocol magic numbers.
//!
//! # Sizing Policy
//!
//! The values favor predictable bounded memory over unbounded allocation. Socket
//! buffers are large enough for common POSIX workloads, while router/device
//! queues absorb short scheduling bursts without turning every packet path into
//! a heap allocation site. If a value is raised, consider the total cost across
//! all sockets or all devices, not only the cost of one queue.

pub const STANDARD_MTU: usize = 1500;

pub const TCP_RX_BUF_LEN: usize = 64 * 1024;
pub const TCP_TX_BUF_LEN: usize = 64 * 1024;
pub const UDP_RX_BUF_LEN: usize = 64 * 1024;
pub const UDP_TX_BUF_LEN: usize = 64 * 1024;
pub const RAW_RX_BUF_LEN: usize = 64 * 1024;
pub const RAW_TX_BUF_LEN: usize = 64 * 1024;
pub const LISTEN_QUEUE_SIZE: usize = 512;

pub const SOCKET_BUFFER_SIZE: usize = 64;

/// Shared device-to-router RX queue capacity.
///
/// This queue absorbs packets produced by per-device RX workers before the
/// single smoltcp protocol core can drain them. It is intentionally larger than
/// the smoltcp-facing packet buffer: internet downloads and APK index fetches
/// can deliver short RX bursts faster than the net-poll worker gets scheduled,
/// especially on single-core QEMU targets.
///
/// 256 slots × 1500 bytes = 384 KiB total for the shared RX queue. The queue is
/// still bounded, but large enough to avoid turning ordinary TCP burstiness
/// into packet loss.
pub const DEVICE_RX_QUEUE_SIZE: usize = 256;

/// Per-device TX queue capacity.
///
/// Sized to absorb bursty traffic without drops while keeping memory bounded.
/// 128 slots × 1500 bytes = 192KB per network device (acceptable for embedded).
///
/// Rationale:
/// - At 1Gbps, 128 packets = ~1.5ms of buffering
/// - Handles typical burst scenarios (ARP resolution, TCP slow start)
/// - Reduces packet loss under momentary TX worker scheduling delays
pub const DEVICE_TX_QUEUE_SIZE: usize = 128;

/// Number of outbound packets that can be queued while waiting for ARP
/// resolution of the next hop.
///
/// 32 was too small in practice: applications that fan out 10-20 concurrent
/// TCP connections at startup (browsers, HTTP clients, CLIs that talk to an
/// AI/cloud API) overflow the queue with their first SYN burst before the
/// gateway ARP reply arrives, and the excess packets are silently dropped.
/// Long-running streams that outlive [`NEIGHBOR_TTL`] also re-enter the
/// queue when the cached neighbour expires mid-flow.
///
/// 128 slots (~189 KiB at 1514 bytes per packet) covers the bursts these
/// applications produce while keeping the boot-time heap footprint small
/// enough for the 128 MiB QEMU defaults that the test rigs use.
pub const ETHERNET_MAX_PENDING_PACKETS: usize = 128;
