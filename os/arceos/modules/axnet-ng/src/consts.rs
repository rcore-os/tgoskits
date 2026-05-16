macro_rules! env_or_default {
    ($key:literal) => {
        match option_env!($key) {
            Some(val) => val,
            None => "",
        }
    };
}

pub const IP: &str = env_or_default!("AX_IP");
pub const GATEWAY: &str = env_or_default!("AX_GW");
pub const IP_PREFIX: u8 = 24;

pub const STANDARD_MTU: usize = 1500;

pub const TCP_RX_BUF_LEN: usize = 64 * 1024;
pub const TCP_TX_BUF_LEN: usize = 64 * 1024;
pub const UDP_RX_BUF_LEN: usize = 64 * 1024;
pub const UDP_TX_BUF_LEN: usize = 64 * 1024;
pub const RAW_RX_BUF_LEN: usize = 64 * 1024;
pub const RAW_TX_BUF_LEN: usize = 64 * 1024;
pub const LISTEN_QUEUE_SIZE: usize = 512;

pub const SOCKET_BUFFER_SIZE: usize = 64;
/// Number of outbound packets queued while waiting for ARP resolution of the
/// next-hop MAC address.  32 was exhausted immediately when an application
/// (e.g. jcode) opens 10-20 concurrent TCP connections at startup: each SYN
/// queues before the gateway ARP reply arrives.  If the ARP cache also expires
/// mid-stream (see NEIGHBOR_TTL), a burst of TCP ACKs refills the queue.
/// 256 × 1514 ≈ 384 KiB is comfortable for these workloads.
pub const ETHERNET_MAX_PENDING_PACKETS: usize = 256;
