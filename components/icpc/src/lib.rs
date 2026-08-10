//! Industrial Control Plane Communication (icpc) protocol core.
//!
//! Wire format (little-endian, 24-byte header + payload):
//!
//! ```text
//! ver | type | flags | rsvd | seq(u32) | timestamp_ns(u64)
//! payload_len(u16) | err_code(u16) | crc32(u32) | payload...
//! ```

#![cfg_attr(not(test), no_std)]

mod crc32;
mod flags;
mod header;
mod message;
mod reliability;

pub use crc32::crc32;
pub use flags::NEED_ACK;
pub use header::{HEADER_LEN, Header, ProtocolError};
pub use message::{Message, MessageType, decode, encode};
pub use reliability::{
    DEDUP_WINDOW, DEFAULT_HEARTBEAT_INTERVAL_MS, DEFAULT_HEARTBEAT_MISS_THRESHOLD,
    DEFAULT_INITIAL_TIMEOUT_MS, DEFAULT_MAX_RETRIES, DEFAULT_MAX_TIMEOUT_MS, DedupWindow,
    HeartbeatConfig, HeartbeatState, LinkState, StopWaitConfig, StopWaitState,
    ack_type_for_request,
};
