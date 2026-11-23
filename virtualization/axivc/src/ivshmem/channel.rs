use core::{
    marker::PhantomData,
    mem::offset_of,
    sync::atomic::{AtomicU32, Ordering, fence},
};

use super::{
    IVSHMEM_MESSAGE_CHANNEL_SIZE, IVSHMEM_MESSAGE_HEADER_SIZE, IVSHMEM_MESSAGE_MAGIC,
    IVSHMEM_MESSAGE_VERSION, IVSHMEM_SESSION_ACKNOWLEDGED, IVSHMEM_SESSION_INVALID,
    IVSHMEM_SESSION_SEEKING, IvshmemMessageHeaderLayout,
};
use crate::{IVC_RING_CAPACITY, IVC_SLOT_SIZE};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct IvshmemHeader {
    pub(super) magic: u32,
    pub(super) version: u32,
    pub(super) header_size: u32,
    pub(super) channel_size: u32,
    pub(super) slot_size: u32,
    pub(super) slot_count: u32,
    pub(super) incarnation: u32,
    pub(super) peer_incarnation: u32,
    pub(super) session_state: u32,
}

impl IvshmemHeader {
    pub(super) fn layout_matches(self) -> bool {
        self.magic == IVSHMEM_MESSAGE_MAGIC
            && self.version == IVSHMEM_MESSAGE_VERSION
            && self.header_size as usize == IVSHMEM_MESSAGE_HEADER_SIZE
            && self.channel_size as usize == IVSHMEM_MESSAGE_CHANNEL_SIZE
            && self.slot_size as usize == IVC_SLOT_SIZE
            && self.slot_count as usize == IVC_RING_CAPACITY
            && matches!(
                self.session_state,
                IVSHMEM_SESSION_SEEKING | IVSHMEM_SESSION_ACKNOWLEDGED
            )
    }

    pub(super) fn acknowledged(self) -> bool {
        self.session_state == IVSHMEM_SESSION_ACKNOWLEDGED
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct IvshmemSessionHeader {
    pub(super) incarnation: u32,
    pub(super) peer_incarnation: u32,
    pub(super) session_state: u32,
}

impl IvshmemSessionHeader {
    pub(super) fn acknowledged(self) -> bool {
        self.session_state == IVSHMEM_SESSION_ACKNOWLEDGED
    }
}

/// Owner-writable output-region channel.
///
/// This type is the only one that uses `AtomicU32::from_ptr`, writes slots, or
/// performs release stores. The mapping contract guarantees that its complete
/// range is readable and writable.
#[derive(Clone, Copy)]
pub(crate) struct IvshmemLocalChannel<'a> {
    base: *const u8,
    _mapping: PhantomData<&'a ()>,
}

// SAFETY: the attach contract keeps the mapping valid for `'a` and assigns one
// writer. Sender and receiver operations update disjoint cursor/slot fields;
// release/acquire atomics prevent slot reuse while a consumer holds credit.
unsafe impl<'a> Send for IvshmemLocalChannel<'a> {}
// SAFETY: shared use is limited to atomic accesses on disjoint fields and the
// sole producer/consumer slot protocol described above.
unsafe impl<'a> Sync for IvshmemLocalChannel<'a> {}

impl<'a> IvshmemLocalChannel<'a> {
    pub(super) const fn new(base: *const u8) -> Self {
        Self {
            base,
            _mapping: PhantomData,
        }
    }

    pub(super) fn read_header(self) -> Option<IvshmemHeader> {
        read_header_snapshot(
            || {
                self.load_atomic(
                    offset_of!(IvshmemMessageHeaderLayout, session_state),
                    Ordering::Acquire,
                )
            },
            |offset| self.load_atomic(offset, Ordering::Relaxed),
        )
    }

    pub(super) fn read_session(self) -> Option<IvshmemSessionHeader> {
        read_session_snapshot(
            || {
                self.load_atomic(
                    offset_of!(IvshmemMessageHeaderLayout, session_state),
                    Ordering::Acquire,
                )
            },
            |offset| self.load_atomic(offset, Ordering::Relaxed),
        )
    }

    pub(super) fn incarnation(self) -> u32 {
        self.load_atomic(
            offset_of!(IvshmemMessageHeaderLayout, incarnation),
            Ordering::Acquire,
        )
    }

    pub(super) fn publish_session(self, incarnation: u32) {
        // The owner makes the previous generation unusable before resetting
        // cursors. One Release store of SEEKING publishes every field below.
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, session_state),
            IVSHMEM_SESSION_INVALID,
            Ordering::Release,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, magic),
            IVSHMEM_MESSAGE_MAGIC,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, version),
            IVSHMEM_MESSAGE_VERSION,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, header_size),
            IVSHMEM_MESSAGE_HEADER_SIZE as u32,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, channel_size),
            IVSHMEM_MESSAGE_CHANNEL_SIZE as u32,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, slot_size),
            IVC_SLOT_SIZE as u32,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, slot_count),
            IVC_RING_CAPACITY as u32,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, incarnation),
            incarnation,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, peer_incarnation),
            0,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, tx_tail),
            0,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, rx_head),
            0,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, session_state),
            IVSHMEM_SESSION_SEEKING,
            Ordering::Release,
        );
    }

    pub(super) fn acknowledge_peer(self, peer_incarnation: u32) {
        self.reset_cursors();
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, peer_incarnation),
            peer_incarnation,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, session_state),
            IVSHMEM_SESSION_ACKNOWLEDGED,
            Ordering::Release,
        );
    }

    pub(super) fn load(self, offset: usize, ordering: Ordering) -> u32 {
        // SAFETY: attach validates alignment and that the whole local output is
        // writable. Every offset is an aligned control field in that mapping.
        unsafe {
            AtomicU32::from_ptr(self.base.add(offset).cast_mut().cast::<u32>()).load(ordering)
        }
    }

    pub(super) fn store(self, offset: usize, value: u32, ordering: Ordering) {
        // SAFETY: this is the local writable channel, with the same aligned
        // in-range mapping guarantee as `load`.
        unsafe {
            AtomicU32::from_ptr(self.base.add(offset).cast_mut().cast::<u32>())
                .store(value, ordering);
        }
    }

    pub(super) fn write_slot(self, index: usize, slot: &[u8; IVC_SLOT_SIZE]) {
        debug_assert!(index < IVC_RING_CAPACITY);
        let slot_offset = IVSHMEM_MESSAGE_HEADER_SIZE + index * IVC_SLOT_SIZE;
        for (word_index, word) in slot.as_chunks::<4>().0.iter().enumerate() {
            let value = u32::from_ne_bytes(*word);
            // SAFETY: slots are 256-byte aligned, word offsets are 4-byte
            // aligned, and the local output covers the whole channel.
            unsafe {
                AtomicU32::from_ptr(
                    self.base
                        .add(slot_offset + word_index * 4)
                        .cast_mut()
                        .cast::<u32>(),
                )
                .store(value, Ordering::Relaxed);
            }
        }
    }

    fn load_atomic(self, offset: usize, ordering: Ordering) -> u32 {
        self.load(offset, ordering)
    }

    fn reset_cursors(self) {
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, tx_tail),
            0,
            Ordering::Relaxed,
        );
        self.store(
            offset_of!(IvshmemMessageHeaderLayout, rx_head),
            0,
            Ordering::Relaxed,
        );
    }
}

/// Peer-owned output-region channel mapped read-only here.
///
/// This type never uses `AtomicU32::from_ptr`, never stores, and never performs
/// read-modify-write operations. All accesses are shared `AtomicU32` loads.
#[derive(Clone, Copy)]
pub(crate) struct IvshmemPeerChannel<'a> {
    base: *const u8,
    _mapping: PhantomData<&'a ()>,
}

// SAFETY: the attach contract keeps the read-only mapping valid for `'a`.
// Concurrent peer writes use the same aligned atomic objects and no handle
// creates ordinary references or non-atomic accesses to those words.
unsafe impl<'a> Send for IvshmemPeerChannel<'a> {}
// SAFETY: only shared atomic loads are exposed, including while the peer writes
// through its owner mapping.
unsafe impl<'a> Sync for IvshmemPeerChannel<'a> {}

impl<'a> IvshmemPeerChannel<'a> {
    pub(super) const fn new(base: *const u8) -> Self {
        Self {
            base,
            _mapping: PhantomData,
        }
    }

    pub(super) fn read_header(self) -> Option<IvshmemHeader> {
        read_header_snapshot(
            || self.load_acquire(offset_of!(IvshmemMessageHeaderLayout, session_state)),
            |offset| self.load_relaxed(offset),
        )
    }

    pub(super) fn read_session(self) -> Option<IvshmemSessionHeader> {
        read_session_snapshot(
            || self.load_acquire(offset_of!(IvshmemMessageHeaderLayout, session_state)),
            |offset| self.load_relaxed(offset),
        )
    }

    pub(super) fn load_relaxed(self, offset: usize) -> u32 {
        self.atomic(offset).load(Ordering::Relaxed)
    }

    pub(super) fn load_acquire(self, offset: usize) -> u32 {
        let value = self.load_relaxed(offset);
        // Core's documented read-only-atomic pattern: only small Relaxed loads
        // are guaranteed on read-only memory; the Acquire fence supplies the
        // synchronization edge without requiring a stronger memory access.
        fence(Ordering::Acquire);
        value
    }

    pub(super) fn read_slot(self, index: usize, slot: &mut [u8; IVC_SLOT_SIZE]) {
        debug_assert!(index < IVC_RING_CAPACITY);
        let slot_offset = IVSHMEM_MESSAGE_HEADER_SIZE + index * IVC_SLOT_SIZE;
        for (word_index, word) in slot.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            *word = self
                .load_relaxed(slot_offset + word_index * 4)
                .to_ne_bytes();
        }
    }

    fn atomic(self, offset: usize) -> &'a AtomicU32 {
        // SAFETY: attach validates that the base is 256-byte aligned, covers
        // the complete channel, and refers to read-only shared memory here.
        // Every offset is a 4-byte-aligned atomic word. Only atomic loads are
        // performed; no RMW operation requires writable backing.
        unsafe { &*self.base.add(offset).cast::<AtomicU32>() }
    }
}

fn read_header_snapshot(
    mut load_state: impl FnMut() -> u32,
    mut load_relaxed: impl FnMut(usize) -> u32,
) -> Option<IvshmemHeader> {
    // `None` is intentionally context-free: handshake callers treat it as
    // retryable publication progress, while connected endpoints treat it as a
    // lost session because their shared header no longer has a transient state
    // transition to complete.
    let state_before = load_state();
    if state_before == IVSHMEM_SESSION_INVALID {
        return None;
    }
    let incarnation_before = load_relaxed(offset_of!(IvshmemMessageHeaderLayout, incarnation));
    let header = IvshmemHeader {
        magic: load_relaxed(offset_of!(IvshmemMessageHeaderLayout, magic)),
        version: load_relaxed(offset_of!(IvshmemMessageHeaderLayout, version)),
        header_size: load_relaxed(offset_of!(IvshmemMessageHeaderLayout, header_size)),
        channel_size: load_relaxed(offset_of!(IvshmemMessageHeaderLayout, channel_size)),
        slot_size: load_relaxed(offset_of!(IvshmemMessageHeaderLayout, slot_size)),
        slot_count: load_relaxed(offset_of!(IvshmemMessageHeaderLayout, slot_count)),
        incarnation: incarnation_before,
        peer_incarnation: load_relaxed(offset_of!(IvshmemMessageHeaderLayout, peer_incarnation)),
        session_state: state_before,
    };
    let incarnation_after = load_relaxed(offset_of!(IvshmemMessageHeaderLayout, incarnation));
    let state_after = load_state();
    (state_before == state_after
        && incarnation_before == incarnation_after
        && header.incarnation == incarnation_after)
        .then_some(header)
}

fn read_session_snapshot(
    mut load_state: impl FnMut() -> u32,
    mut load_relaxed: impl FnMut(usize) -> u32,
) -> Option<IvshmemSessionHeader> {
    // The hot path checks only generation, reciprocal acknowledgement, and
    // shared state. Layout constants were validated while attaching/connecting.
    let state_before = load_state();
    if state_before == IVSHMEM_SESSION_INVALID {
        return None;
    }
    let incarnation_before = load_relaxed(offset_of!(IvshmemMessageHeaderLayout, incarnation));
    let header = IvshmemSessionHeader {
        incarnation: incarnation_before,
        peer_incarnation: load_relaxed(offset_of!(IvshmemMessageHeaderLayout, peer_incarnation)),
        session_state: state_before,
    };
    let incarnation_after = load_relaxed(offset_of!(IvshmemMessageHeaderLayout, incarnation));
    let state_after = load_state();
    (state_before == state_after
        && incarnation_before == incarnation_after
        && header.incarnation == incarnation_after)
        .then_some(header)
}
