#![no_std]

//! Shared-memory protocol helpers for AxVisor inter-VM communication.

#[cfg(test)]
extern crate std;

mod endpoint;
mod event;
mod message;
mod region;
mod ring;

pub use endpoint::{IvcConsumer, IvcEndpoints, IvcProducer};
pub use event::{IvcPeerEventWaiter, fallback_poll, record_peer_event};
pub use message::{IvcMessage, IvcMessageKind};
pub use region::{
    IVC_PUBLISHER_TO_SUBSCRIBER_RING_OFFSET as IVC_REGION_PUBLISHER_TO_SUBSCRIBER_OFFSET,
    IVC_REGION_HEADER_SIZE, IVC_REGION_TOTAL_SIZE as IVC_REGION_SIZE,
    IVC_RING_HEADER_SIZE as IVC_REGION_RING_SIZE,
    IVC_SUBSCRIBER_TO_PUBLISHER_RING_OFFSET as IVC_REGION_SUBSCRIBER_TO_PUBLISHER_OFFSET,
    IvcRegion,
};
pub use ring::{IvcRingDirection, IvcRingError};

/// Magic value stored in `IvcRegionHeader`.
pub const IVC_REGION_MAGIC: u32 = 0x4956_4332;
/// Current shared-memory protocol version.
pub const IVC_REGION_VERSION: u16 = 2;
/// Fixed-slot SPSC ring protocol feature bit.
pub const IVC_REGION_FEATURE_SPSC_FIXED_SLOTS: u32 = 1;
/// Fixed slot payload capacity.
pub const IVC_SLOT_PAYLOAD_SIZE: usize = 48;
/// Number of slots per one-way ring.
pub const IVC_RING_CAPACITY: usize = 16;
/// Default bounded polling budget used after no peer IRQ event is observed.
pub const IVC_DEFAULT_FALLBACK_POLL_ROUNDS: usize = 100_000;

#[cfg(test)]
mod tests;
