#![no_std]

// Shared constants for the net_stats eBPF program and userspace loader.
// Both crates depend on net_stats-common so counter indices stay in sync.

/// Counter index: transmitted packets.
pub const TX_PKTS: u32 = 0;
/// Counter index: transmitted bytes (L2 frame length).
pub const TX_BYTES: u32 = 1;
/// Counter index: received packets.
pub const RX_PKTS: u32 = 2;
/// Counter index: received bytes (L2 frame length).
pub const RX_BYTES: u32 = 3;
/// Total number of counter slots in the `NETSTATS` BPF map.
pub const MAP_SIZE: u32 = 4;
