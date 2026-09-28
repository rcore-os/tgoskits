//! Typed failures of ivshmem link configuration and BAR semantics.

use alloc::string::String;

/// Errors reported while reserving peers or accessing ivshmem BARs.
///
/// Configuration failures happen before any VM runs; guest BAR accesses map
/// onto the device access errors of the calling adapter instead.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum IvshmemError {
    /// The requested peer is outside the current link profile.
    #[error("ivshmem peer {peer} is outside the current profile of {max_peers} peers")]
    PeerOutOfProfile {
        /// The requested peer ID.
        peer: u16,
        /// The peer count of the link profile.
        max_peers: u16,
    },
    /// The peer slot is already held by another reservation.
    #[error("ivshmem peer {peer} of link {link} is already reserved")]
    PeerAlreadyReserved {
        /// The link that owns the peer slot.
        link: u32,
        /// The requested peer ID.
        peer: u16,
    },
    /// The reservation was released earlier; the slot stays blocked until the
    /// whole link is destroyed.
    #[error(
        "ivshmem peer {peer} of link {link} was released earlier and cannot be reserved again \
         until the whole link is destroyed"
    )]
    PeerRetired {
        /// The link that owns the peer slot.
        link: u32,
        /// The retired peer ID.
        peer: u16,
    },
    /// The peer slot is already bound to one runtime endpoint.
    #[error("ivshmem peer {peer} of link {link} is already attached to a runtime endpoint")]
    PeerAlreadyAttached {
        /// The link that owns the peer slot.
        link: u32,
        /// The attached peer ID.
        peer: u16,
    },
    /// The peer slot has no active reservation, so it cannot be attached.
    #[error("ivshmem peer {peer} of link {link} has no active reservation")]
    PeerNotReserved {
        /// The link that owns the peer slot.
        link: u32,
        /// The unreserved peer ID.
        peer: u16,
    },
    /// Shared backing or peer-slot allocation failed.
    #[error("ivshmem allocation for {operation} failed")]
    AllocationFailed {
        /// The operation that attempted the allocation.
        operation: &'static str,
    },
    /// A BAR0 access is not an aligned 32-bit access inside the register page.
    #[error(
        "ivshmem register access at offset {offset:#x} is not an aligned 32-bit offset inside the \
         {page_size:#x}-byte register page"
    )]
    InvalidRegisterAccess {
        /// The rejected BAR0 offset.
        offset: u64,
        /// The register page size that defines the valid range.
        page_size: u64,
    },
    /// A BAR2 access uses a width outside the supported set.
    #[error("ivshmem shared-memory width {width} is not one of 1, 2, 4 or 8 bytes")]
    InvalidSharedMemoryWidth {
        /// The rejected access width in bytes.
        width: usize,
    },
    /// A BAR2 access falls outside the shared region.
    #[error(
        "ivshmem shared-memory access at offset {offset:#x} with {width} bytes exceeds the \
         {size:#x}-byte region"
    )]
    SharedMemoryOutOfRange {
        /// The rejected BAR2 offset.
        offset: u64,
        /// The requested access width in bytes.
        width: usize,
        /// The shared region size that defines the valid range.
        size: u64,
    },
    /// The BAR2 size cannot host the requested layout.
    #[error("ivshmem BAR2 layout is invalid: {detail}")]
    InvalidLayout {
        /// Diagnostic detail describing the rejected derivation.
        detail: String,
    },
    /// A reservation declared a different link profile than an earlier one.
    #[error("ivshmem link {link} profile mismatch: {detail}")]
    LinkProfileMismatch {
        /// The link whose profile was already fixed.
        link: u32,
        /// Diagnostic detail comparing both profiles.
        detail: String,
    },
    /// A link profile violates the validation rules of `LinkProfile::new`.
    #[error("ivshmem link profile is invalid: {detail}")]
    InvalidProfile {
        /// Diagnostic detail describing the rejected field.
        detail: String,
    },
    /// A reservation or attachment belongs to a previous link generation.
    #[error(
        "ivshmem reference to link {link} belongs to generation {reserved}, but the link is now \
         at generation {current}"
    )]
    LinkGenerationChanged {
        /// The link that changed generation.
        link: u32,
        /// The generation captured by the stale reference.
        reserved: u64,
        /// The link's current generation.
        current: u64,
    },
    /// An event sink could not record one delivered event.
    #[error("ivshmem event delivery failed for {operation}: {detail}")]
    EventDeliveryFailed {
        /// The sink operation that failed.
        operation: &'static str,
        /// Diagnostic detail from the sink.
        detail: String,
    },
}
