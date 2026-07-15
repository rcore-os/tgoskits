#![no_std]

//! Shared-memory protocol helpers for AxVisor inter-VM communication.

mod event;
mod message;
mod region;
mod ring;

pub use event::{IvcPeerEventWaiter, fallback_poll, record_peer_event};
pub use message::{IvcMessage, IvcMessageKind};
pub use region::IvcRegion;
pub use ring::{IvcRingDirection, IvcRingError};

/// Magic value stored in `IvcRegionHeader`.
pub const IVC_REGION_MAGIC: u32 = 0x4956_4332;
/// Current shared-memory protocol version.
pub const IVC_REGION_VERSION: u16 = 2;
/// Fixed slot payload capacity.
pub const IVC_SLOT_PAYLOAD_SIZE: usize = 48;
/// Number of slots per one-way ring.
pub const IVC_RING_CAPACITY: usize = 16;
/// Default bounded polling budget used after no peer IRQ event is observed.
pub const IVC_DEFAULT_FALLBACK_POLL_ROUNDS: usize = 100_000;

#[cfg(test)]
mod tests;
