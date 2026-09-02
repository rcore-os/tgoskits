#![no_std]

//! Shared-memory logical-message protocol helpers for AxVisor IVC channels.
//!
//! The crate transports allocation-free, fragmented messages over two opaque
//! single-producer/single-consumer cell rings. Hypercalls, GPA mapping, IRQ
//! registration, blocking waits, and application payload semantics remain in
//! guest OS glue.

#[cfg(test)]
extern crate std;

mod endpoint;
mod event;
mod message;
mod region;
mod ring;

pub use endpoint::IvcEndpoints;
pub use event::{IvcPeerEventWaiter, fallback_poll, record_peer_event};
pub use message::{
    IvcMessageError, IvcMessageId, IvcMessageMeta, IvcMessageReceiver, IvcMessageSender,
    IvcReceiveProgress, IvcSendProgress,
};
pub use region::IvcRegion;
pub use ring::IvcRingDirection;

/// Magic value stored in the IVC region header.
pub const IVC_REGION_MAGIC: u32 = 0x4956_4332;
/// Current incompatible shared-memory queue-layout version.
pub const IVC_REGION_VERSION: u16 = 3;
/// Size in bytes of one opaque queue cell.
pub const IVC_CELL_SIZE: usize = 64;
/// Size in bytes of the Message V1 frame header.
pub const IVC_MESSAGE_HEADER_SIZE: usize = 24;
/// Maximum logical-message fragment carried by one V1 cell.
pub const IVC_CELL_FRAGMENT_CAPACITY: usize = IVC_CELL_SIZE - IVC_MESSAGE_HEADER_SIZE;
/// Number of cells per one-way ring.
pub const IVC_RING_CAPACITY: usize = 16;
/// Default bounded polling budget used after no peer IRQ event is observed.
pub const IVC_DEFAULT_FALLBACK_POLL_ROUNDS: usize = 100_000;

#[cfg(test)]
mod tests;
