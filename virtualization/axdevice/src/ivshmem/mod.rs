//! OS-agnostic ivshmem link semantics shared by peer endpoints.
//!
//! This module owns the device-side state that peers of one ivshmem link
//! share: the register page, the shared-memory backing, and the peer
//! lifecycle. It deliberately knows nothing about PCI config space, BAR
//! routing, guest address spaces, or TOML options; the AxVM adapter binds
//! this semantics to a [`crate::pci::PciFunction`].
//!
//! Locking: the registry is only taken on the configuration path with the
//! `registry -> link` order. Register and backing locks are independent and
//! are never nested; doorbell events cross these locks as value objects and
//! are delivered to target sinks outside every lock, so neither the link
//! peer-table lock nor a register lock is ever held while a sink runs.

mod backing;
mod doorbell;
mod error;
mod layout;
mod link;
mod plan;
mod registers;

pub use backing::{BackingAllocation, SharedBackingAllocator, SharedBarBacking};
pub use doorbell::{Doorbell, DoorbellEvent, IvshmemEventSink};
pub use error::IvshmemError;
pub use layout::{Bar2Section, IvshmemMemoryLayout, SectionDesc};
pub use link::{
    IvshmemLink, IvshmemLinkRegistry, LinkGeneration, LinkId, LinkProfile, MAX_PEERS_LIMIT,
    PeerAttachment, PeerId, PeerReservation,
};
pub use plan::IvshmemDirectPlan;
pub use registers::{
    DOORBELL_OFFSET, EVENT_STATUS_OFFSET, ID_OFFSET, INTERRUPT_CONTROL_OFFSET, IvshmemRegisters,
    MAXIMUM_PEERS_OFFSET, REGISTER_PAGE_SIZE, STATE_OFFSET,
};

/// Shared-memory BAR size of the current device profile.
pub const SHARED_MEMORY_SIZE: u64 = 0x1_0000;
