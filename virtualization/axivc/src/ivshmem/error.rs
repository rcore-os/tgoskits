use thiserror::Error;

use super::IvshmemSessionState;

/// One of the two owner-only output regions supplied to an ivshmem message port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IvshmemRegionRole {
    /// The output region owned and writable by this peer.
    LocalOutput,
    /// The output region owned by the peer and mapped read-only here.
    PeerOutput,
}

/// Errors returned while attaching or pairing an ivshmem message port.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
pub enum IvshmemMessageError {
    /// An output-region pointer is null.
    #[error("the {role:?} pointer is null")]
    NullRegion {
        /// Region that failed validation.
        role: IvshmemRegionRole,
    },
    /// An output-region pointer does not meet the required alignment.
    #[error("the {role:?} pointer is not {alignment}-byte aligned")]
    MisalignedRegion {
        /// Region that failed validation.
        role: IvshmemRegionRole,
        /// Required byte alignment.
        alignment: usize,
    },
    /// An output-region range is shorter than the fixed channel layout.
    #[error("the {role:?} region has {provided} bytes but requires at least {required}")]
    RegionTooSmall {
        /// Region that failed validation.
        role: IvshmemRegionRole,
        /// Minimum required channel size.
        required: usize,
        /// Size supplied by the caller.
        provided: usize,
    },
    /// An output-region range overflows the address space.
    #[error("the {role:?} region range overflows the address space")]
    RegionRangeOverflow {
        /// Region that failed validation.
        role: IvshmemRegionRole,
    },
    /// The two direct-mapped output ranges overlap.
    #[error("the local and peer output regions overlap")]
    OverlappingRegions,
    /// A published channel header describes a different shared-memory layout.
    #[error(
        "the {role:?} region has an unsupported ivshmem message layout: magic={magic:#x}, \
         version={version}, header_size={header_size}, channel_size={channel_size}, \
         slot_size={slot_size}, slot_count={slot_count}, session_state={session_state}"
    )]
    UnsupportedLayout {
        /// Region that failed validation.
        role: IvshmemRegionRole,
        /// Header magic observed in shared memory.
        magic: u32,
        /// Layout version observed in shared memory.
        version: u32,
        /// Header size observed in shared memory.
        header_size: u32,
        /// Channel size observed in shared memory.
        channel_size: u32,
        /// Slot size observed in shared memory.
        slot_size: u32,
        /// Slot count observed in shared memory.
        slot_count: u32,
        /// Session state observed in shared memory.
        session_state: u32,
    },
    /// The local incarnation cannot be incremented without wrapping to zero.
    #[error("the ivshmem incarnation space is exhausted at {current}")]
    IncarnationExhausted {
        /// Current incarnation stored in the local output region.
        current: u32,
    },
    /// Handshake state does not permit exposing message endpoints.
    #[error("the ivshmem message session is not ready: {state:?}")]
    SessionNotReady {
        /// Current session state.
        state: IvshmemSessionState,
    },
    /// This port already returned one set of endpoints.
    #[error("this ivshmem message port is already connected")]
    SessionAlreadyConnected,
}
