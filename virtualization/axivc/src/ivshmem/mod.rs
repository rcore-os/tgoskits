//! Message V1 transport over owner-only ivshmem output regions.
//!
//! Each peer owns one writable output region and maps the peer's corresponding
//! region read-only. A region contains the local TX tail and slots plus the
//! local RX head/credit for traffic produced by the peer. This split keeps
//! every writable cursor in the writer's own region; neither peer constructs a
//! writable view of the other peer's section.
//!
//! One [`IvshmemMessagePort`] represents one per-peer lane. Device discovery,
//! BAR mapping, doorbells, and State transitions remain outside this crate.
//!
//! The first 256 bytes of every output region contain `u32` atomics at these
//! byte offsets: magic 0, version 4, header size 8, channel size 12, slot size
//! 16, slot count 20, incarnation 24, peer incarnation 28, session state 32,
//! TX tail 36, and RX head 40. Message V1 slots begin at byte 256. Each slot is
//! 256-byte aligned, so the channel occupies 8448 bytes. Control words use the
//! peers' common native byte order; Message V1 frame fields remain
//! little-endian.

mod backend;
mod channel;
mod error;

use core::{
    mem::{align_of, offset_of, size_of},
    num::NonZeroU32,
};

pub(crate) use backend::{IvshmemSlotConsumer, IvshmemSlotProducer};
pub(crate) use channel::{IvshmemLocalChannel, IvshmemPeerChannel};
pub use error::{IvshmemMessageError, IvshmemRegionRole};

use crate::{IVC_RING_CAPACITY, IVC_SLOT_SIZE, IvcEndpoints, ivshmem::channel::IvshmemHeader};

/// Wire header shape used only for compile-time offsets and size/alignment.
/// Mapped bytes are always accessed field-by-field through atomics.
#[repr(C, align(256))]
struct IvshmemMessageHeaderLayout {
    magic: u32,
    version: u32,
    header_size: u32,
    channel_size: u32,
    slot_size: u32,
    slot_count: u32,
    incarnation: u32,
    peer_incarnation: u32,
    session_state: u32,
    tx_tail: u32,
    rx_head: u32,
}

/// Magic identifying this ivshmem message-channel layout.
const IVSHMEM_MESSAGE_MAGIC: u32 = 0x4956_5348;
/// Version of the ivshmem message-channel layout and handshake.
const IVSHMEM_MESSAGE_VERSION: u32 = 1;
/// Wire value for an unpublished or invalid session.
const IVSHMEM_SESSION_INVALID: u32 = 0;
/// Wire value for a local incarnation waiting for a peer.
const IVSHMEM_SESSION_SEEKING: u32 = 1;
/// Wire value after recording a stable peer incarnation.
const IVSHMEM_SESSION_ACKNOWLEDGED: u32 = 2;
/// Required alignment of each owner output region passed to this module.
const IVSHMEM_MESSAGE_ALIGNMENT: usize = align_of::<IvshmemMessageHeaderLayout>();
/// Fixed header bytes at the start of each owner output region.
const IVSHMEM_MESSAGE_HEADER_SIZE: usize = size_of::<IvshmemMessageHeaderLayout>();
/// Bytes used at the start of each ivshmem peer output section.
const IVSHMEM_MESSAGE_CHANNEL_SIZE: usize =
    IVSHMEM_MESSAGE_HEADER_SIZE + IVC_RING_CAPACITY * IVC_SLOT_SIZE;

const _: () = {
    assert!(offset_of!(IvshmemMessageHeaderLayout, magic) == 0);
    assert!(offset_of!(IvshmemMessageHeaderLayout, version) == 4);
    assert!(offset_of!(IvshmemMessageHeaderLayout, header_size) == 8);
    assert!(offset_of!(IvshmemMessageHeaderLayout, channel_size) == 12);
    assert!(offset_of!(IvshmemMessageHeaderLayout, slot_size) == 16);
    assert!(offset_of!(IvshmemMessageHeaderLayout, slot_count) == 20);
    assert!(offset_of!(IvshmemMessageHeaderLayout, incarnation) == 24);
    assert!(offset_of!(IvshmemMessageHeaderLayout, peer_incarnation) == 28);
    assert!(offset_of!(IvshmemMessageHeaderLayout, session_state) == 32);
    assert!(offset_of!(IvshmemMessageHeaderLayout, tx_tail) == 36);
    assert!(offset_of!(IvshmemMessageHeaderLayout, rx_head) == 40);
    assert!(IVSHMEM_MESSAGE_ALIGNMENT == 256);
    assert!(IVSHMEM_MESSAGE_HEADER_SIZE == 256);
    assert!(IVSHMEM_MESSAGE_CHANNEL_SIZE == 8448);
};

/// Observable lifecycle of one ivshmem message port.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IvshmemSessionState {
    /// [`IvshmemMessagePort::begin_session`] has not run for this port.
    NotStarted,
    /// No stable, compatible peer incarnation has been observed yet.
    WaitingForPeer {
        /// Local incarnation published by this port.
        local: NonZeroU32,
    },
    /// This side acknowledged the peer but the reciprocal acknowledgement is
    /// not yet visible.
    WaitingForPeerAcknowledgement {
        /// Local incarnation published by this port.
        local: NonZeroU32,
        /// Stable peer incarnation observed by this port.
        peer: NonZeroU32,
    },
    /// Both sides acknowledged the same incarnations.
    Ready {
        /// Local incarnation published by this port.
        local: NonZeroU32,
        /// Peer incarnation observed by this port.
        peer: NonZeroU32,
    },
    /// The local channel no longer carries the incarnation selected by this
    /// port.
    LocalChanged {
        /// Incarnation selected when this port began.
        expected: NonZeroU32,
        /// Incarnation currently stored in the local region.
        observed: Option<NonZeroU32>,
    },
    /// A peer changed or reset its incarnation after this port recorded one.
    PeerChanged {
        /// Local incarnation published by this port.
        local: NonZeroU32,
        /// Incarnation previously acknowledged in the local region.
        expected_peer: NonZeroU32,
        /// Incarnation currently visible in the peer region.
        observed_peer: Option<NonZeroU32>,
    },
    /// The peer still has the expected incarnation but no longer acknowledges
    /// this local incarnation.
    PeerAcknowledgementLost {
        /// Local incarnation published by this port.
        local: NonZeroU32,
        /// Peer incarnation previously acknowledged.
        peer: NonZeroU32,
    },
}

/// One side of a full-duplex Message V1 connection over two ivshmem outputs.
///
/// The local output must be writable and the peer output must be mapped
/// read-only. The pair is not cloneable. Construct one port per peer lane and
/// use [`Self::try_connect`] once both peers are ready. After endpoints close,
/// reconnect by dropping the old port and creating a new one for the same
/// quiescent backing; its next [`Self::begin_session`] advances the incarnation.
/// A port that issued endpoints never acknowledges a different incarnation.
pub struct IvshmemMessagePort<'a> {
    local: IvshmemLocalChannel<'a>,
    peer: IvshmemPeerChannel<'a>,
    local_incarnation: Option<NonZeroU32>,
    connected: bool,
}

impl<'a> IvshmemMessagePort<'a> {
    /// Attaches one local writable output region and one peer read-only region.
    ///
    /// Only the first 8448 bytes of each supplied range are interpreted.
    /// Larger 4 KiB-aligned ivshmem output sections are accepted; the
    /// remaining bytes stay outside this protocol.
    ///
    /// # Safety
    ///
    /// The caller must guarantee all of the following:
    ///
    /// - `local_output..local_output + local_output_len` is a valid, writable,
    ///   coherent Normal-cache mapping for `'a`, and the caller has exclusive
    ///   protocol ownership of that output section.
    /// - `peer_output..peer_output + peer_output_len` is a valid read-only
    ///   coherent mapping of the peer's distinct output section for `'a`.
    /// - Both pointers are 256-byte aligned and their ranges do not overlap.
    /// - The peer uses the same layout and the mapping remains alive until the
    ///   port and every endpoint returned from it are dropped.
    /// - No code outside this protocol performs ordinary reads or writes of
    ///   the channel header, cursors, or slots. In particular, guest reset must
    ///   stop old endpoints before a new port begins another session.
    /// - The device layer invalidates doorbells and State before remapping or
    ///   releasing either output section.
    ///
    /// The peer mapping is consumed through shared `AtomicU32` loads only, with
    /// no store or read-modify-write operation. Supported targets must provide
    /// 32-bit atomics and the Rust guaranteed small-atomic load behavior on
    /// read-only memory; Acquire reads use a Relaxed load followed by an
    /// Acquire fence. This protocol supports cache-coherent memory only and
    /// does not provide cache maintenance for non-coherent physical boards.
    pub unsafe fn attach(
        local_output: *mut u8,
        local_output_len: usize,
        peer_output: *const u8,
        peer_output_len: usize,
    ) -> Result<Self, IvshmemMessageError> {
        let local_start = validate_region(
            IvshmemRegionRole::LocalOutput,
            local_output.cast_const(),
            local_output_len,
        )?;
        let peer_start =
            validate_region(IvshmemRegionRole::PeerOutput, peer_output, peer_output_len)?;
        let local_end = local_start.checked_add(local_output_len).ok_or(
            IvshmemMessageError::RegionRangeOverflow {
                role: IvshmemRegionRole::LocalOutput,
            },
        )?;
        let peer_end = peer_start.checked_add(peer_output_len).ok_or(
            IvshmemMessageError::RegionRangeOverflow {
                role: IvshmemRegionRole::PeerOutput,
            },
        )?;
        if local_start < peer_end && peer_start < local_end {
            return Err(IvshmemMessageError::OverlappingRegions);
        }

        Ok(Self {
            local: IvshmemLocalChannel::new(local_output.cast_const()),
            peer: IvshmemPeerChannel::new(peer_output),
            local_incarnation: None,
            connected: false,
        })
    }

    /// Publishes a new nonzero incarnation and resets local cursors.
    ///
    /// A fresh backing is a new session domain and starts from one. Reusing the
    /// same backing increments the previous incarnation and returns
    /// [`IvshmemMessageError::IncarnationExhausted`] rather than wrapping to
    /// zero.
    ///
    /// # Safety
    ///
    /// The caller must be the sole initializer of the local output region for
    /// this generation. No endpoint from a previous generation may remain live
    /// in any address space, and no other code may concurrently update the
    /// local header, cursors, or slots. A reset guest creates a new port and
    /// calls this method before polling the handshake.
    pub unsafe fn begin_session(&mut self) -> Result<NonZeroU32, IvshmemMessageError> {
        if self.connected {
            return Err(IvshmemMessageError::SessionAlreadyConnected);
        }
        let current = self.local.incarnation();
        let next = NonZeroU32::new(current.wrapping_add(1))
            .ok_or(IvshmemMessageError::IncarnationExhausted { current })?;
        self.local.publish_session(next.get());
        self.local_incarnation = Some(next);
        Ok(next)
    }

    /// Advances the nonblocking incarnation handshake.
    ///
    /// Both peers must call this until it returns [`IvshmemSessionState::Ready`].
    /// The method performs no waits and sends no doorbell; the upper layer may
    /// poll it after device State changes or notifications. Starting A before B
    /// leaves A in [`IvshmemSessionState::WaitingForPeer`], so a later B can
    /// complete the same handshake.
    pub fn poll_handshake(&mut self) -> Result<IvshmemSessionState, IvshmemMessageError> {
        let Some(expected_local) = self.local_incarnation else {
            return Ok(IvshmemSessionState::NotStarted);
        };
        let Some(local) = self.local.read_header() else {
            return Ok(IvshmemSessionState::LocalChanged {
                expected: expected_local,
                observed: NonZeroU32::new(self.local.incarnation()),
            });
        };
        validate_header(IvshmemRegionRole::LocalOutput, local)?;
        if local.incarnation != expected_local.get() {
            return Ok(IvshmemSessionState::LocalChanged {
                expected: expected_local,
                observed: NonZeroU32::new(local.incarnation),
            });
        }

        if self.connected {
            return self.connected_state(expected_local, local);
        }

        let Some(peer) = self.stable_peer_header()? else {
            return Ok(IvshmemSessionState::WaitingForPeer {
                local: expected_local,
            });
        };
        validate_header(IvshmemRegionRole::PeerOutput, peer)?;

        let Some(peer_incarnation) = NonZeroU32::new(peer.incarnation) else {
            return Ok(IvshmemSessionState::WaitingForPeer {
                local: expected_local,
            });
        };
        if local.peer_incarnation != peer.incarnation {
            self.local.acknowledge_peer(peer.incarnation);
            return Ok(IvshmemSessionState::WaitingForPeerAcknowledgement {
                local: expected_local,
                peer: peer_incarnation,
            });
        }

        if peer.peer_incarnation == expected_local.get() && peer.acknowledged() {
            Ok(IvshmemSessionState::Ready {
                local: expected_local,
                peer: peer_incarnation,
            })
        } else {
            Ok(IvshmemSessionState::WaitingForPeerAcknowledgement {
                local: expected_local,
                peer: peer_incarnation,
            })
        }
    }

    /// Returns the current handshake/session state without changing it.
    pub fn session_state(&self) -> Result<IvshmemSessionState, IvshmemMessageError> {
        let Some(expected_local) = self.local_incarnation else {
            return Ok(IvshmemSessionState::NotStarted);
        };
        let Some(local) = self.local.read_header() else {
            return Ok(IvshmemSessionState::LocalChanged {
                expected: expected_local,
                observed: NonZeroU32::new(self.local.incarnation()),
            });
        };
        validate_header(IvshmemRegionRole::LocalOutput, local)?;
        if local.incarnation != expected_local.get() {
            return Ok(IvshmemSessionState::LocalChanged {
                expected: expected_local,
                observed: NonZeroU32::new(local.incarnation),
            });
        }

        let Some(expected_peer) = NonZeroU32::new(local.peer_incarnation) else {
            return Ok(IvshmemSessionState::WaitingForPeer {
                local: expected_local,
            });
        };
        let Some(peer) = self.peer.read_session() else {
            return Ok(IvshmemSessionState::PeerChanged {
                local: expected_local,
                expected_peer,
                observed_peer: None,
            });
        };
        if peer.incarnation != expected_peer.get() {
            return Ok(IvshmemSessionState::PeerChanged {
                local: expected_local,
                expected_peer,
                observed_peer: NonZeroU32::new(peer.incarnation),
            });
        }
        if peer.peer_incarnation == expected_local.get() && peer.acknowledged() {
            return Ok(IvshmemSessionState::Ready {
                local: expected_local,
                peer: expected_peer,
            });
        }
        if self.connected {
            Ok(IvshmemSessionState::PeerAcknowledgementLost {
                local: expected_local,
                peer: expected_peer,
            })
        } else {
            Ok(IvshmemSessionState::WaitingForPeerAcknowledgement {
                local: expected_local,
                peer: expected_peer,
            })
        }
    }

    /// Returns whether this port still has a reciprocal, ready session.
    pub fn is_session_current(&self) -> bool {
        matches!(self.session_state(), Ok(IvshmemSessionState::Ready { .. }))
    }

    /// Returns the Message V1 endpoints after both incarnation acknowledgements
    /// become visible.
    ///
    /// The sender reads the peer's RX head, writes only the local TX tail/slots,
    /// and emits Message V1 without duplicating its codec. The receiver reads
    /// only the peer's TX tail/slots and writes the local RX head. After this
    /// method succeeds, the port is permanently bound to the returned session;
    /// later handshake polls may report that session stale but never re-match it.
    pub fn try_connect(&mut self) -> Result<IvcEndpoints<'a>, IvshmemMessageError> {
        if self.connected {
            return Err(IvshmemMessageError::SessionAlreadyConnected);
        }
        let state = self.poll_handshake()?;
        let IvshmemSessionState::Ready { local, peer } = state else {
            return Err(IvshmemMessageError::SessionNotReady { state });
        };
        self.connected = true;
        Ok(IvcEndpoints::new_ivshmem(
            self.local,
            self.peer,
            local.get(),
            peer.get(),
        ))
    }

    fn stable_peer_header(&self) -> Result<Option<IvshmemHeader>, IvshmemMessageError> {
        let Some(first) = self.peer.read_header() else {
            return Ok(None);
        };
        validate_header(IvshmemRegionRole::PeerOutput, first)?;
        let Some(second) = self.peer.read_header() else {
            return Ok(None);
        };
        validate_header(IvshmemRegionRole::PeerOutput, second)?;
        Ok((first.incarnation == second.incarnation).then_some(second))
    }

    fn connected_state(
        &self,
        expected_local: NonZeroU32,
        local: IvshmemHeader,
    ) -> Result<IvshmemSessionState, IvshmemMessageError> {
        let Some(local_incarnation) = NonZeroU32::new(local.incarnation) else {
            return Ok(IvshmemSessionState::LocalChanged {
                expected: expected_local,
                observed: None,
            });
        };
        let Some(expected_peer) = NonZeroU32::new(local.peer_incarnation) else {
            return Ok(IvshmemSessionState::WaitingForPeer {
                local: local_incarnation,
            });
        };
        let Some(peer) = self.peer.read_session() else {
            return Ok(IvshmemSessionState::PeerChanged {
                local: local_incarnation,
                expected_peer,
                observed_peer: None,
            });
        };
        if peer.incarnation != expected_peer.get() {
            return Ok(IvshmemSessionState::PeerChanged {
                local: local_incarnation,
                expected_peer,
                observed_peer: NonZeroU32::new(peer.incarnation),
            });
        }
        if peer.peer_incarnation != local_incarnation.get() || !peer.acknowledged() {
            return Ok(IvshmemSessionState::PeerAcknowledgementLost {
                local: local_incarnation,
                peer: expected_peer,
            });
        }
        Ok(IvshmemSessionState::Ready {
            local: local_incarnation,
            peer: expected_peer,
        })
    }
}

fn validate_region(
    role: IvshmemRegionRole,
    pointer: *const u8,
    length: usize,
) -> Result<usize, IvshmemMessageError> {
    let address = pointer.addr();
    if address == 0 {
        return Err(IvshmemMessageError::NullRegion { role });
    }
    if !address.is_multiple_of(IVSHMEM_MESSAGE_ALIGNMENT) {
        return Err(IvshmemMessageError::MisalignedRegion {
            role,
            alignment: IVSHMEM_MESSAGE_ALIGNMENT,
        });
    }
    if length < IVSHMEM_MESSAGE_CHANNEL_SIZE {
        return Err(IvshmemMessageError::RegionTooSmall {
            role,
            required: IVSHMEM_MESSAGE_CHANNEL_SIZE,
            provided: length,
        });
    }
    address
        .checked_add(length)
        .ok_or(IvshmemMessageError::RegionRangeOverflow { role })?;
    Ok(address)
}

fn validate_header(
    role: IvshmemRegionRole,
    header: IvshmemHeader,
) -> Result<(), IvshmemMessageError> {
    if !header.layout_matches() {
        return Err(IvshmemMessageError::UnsupportedLayout {
            role,
            magic: header.magic,
            version: header.version,
            header_size: header.header_size,
            channel_size: header.channel_size,
            slot_size: header.slot_size,
            slot_count: header.slot_count,
            session_state: header.session_state,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
