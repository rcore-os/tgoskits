pub const STANDARD_MTU: usize = 1500;

pub const TCP_RX_BUF_LEN: usize = 64 * 1024;
pub const TCP_TX_BUF_LEN: usize = 64 * 1024;
pub const UDP_RX_BUF_LEN: usize = 64 * 1024;
pub const UDP_TX_BUF_LEN: usize = 64 * 1024;
pub const RAW_RX_BUF_LEN: usize = 64 * 1024;
pub const RAW_TX_BUF_LEN: usize = 64 * 1024;
pub const LISTEN_QUEUE_SIZE: usize = 512;

pub const SOCKET_BUFFER_SIZE: usize = 64;
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
