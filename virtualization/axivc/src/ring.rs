use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, Ordering},
};

use crate::{IVC_RING_CAPACITY, IVC_SLOT_SIZE};

/// Direction of a one-way IVC ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum IvcRingDirection {
    /// Slots sent by the channel publisher and received by the subscriber.
    PublisherToSubscriber = 1,
    /// Slots sent by the subscriber and received by the publisher.
    SubscriberToPublisher = 2,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum IvcSlotError {
    Full,
}

/// Single-producer, single-consumer opaque-slot ring.
///
/// Slots begin at byte offset 256 within the ring. Each slot is 256-byte
/// aligned, and the complete ring occupies 8448 bytes.
/// With a page-aligned region, no slot straddles a 4 KiB page boundary.
#[repr(C, align(256))]
pub(crate) struct IvcRing {
    direction: AtomicU32,
    capacity: AtomicU32,
    slot_size: AtomicU32,
    head: AtomicU32,
    tail: AtomicU32,
    reserved: [AtomicU32; 3],
    slots: [IvcSlot; IVC_RING_CAPACITY],
}

// SAFETY: Endpoint attachment guarantees exactly one producer and one consumer
// for this ring. The producer exclusively writes an unpublished slot before
// releasing `tail`. The consumer acquires `tail`, exclusively reads that slot,
// and releases `head` only after copying it. The producer acquires `head`
// before reusing a slot, so accesses through each slot's UnsafeCell cannot race.
unsafe impl Sync for IvcRing {}

impl IvcRing {
    pub(crate) fn initialize(&self, direction: IvcRingDirection) {
        self.direction.store(direction as u32, Ordering::Relaxed);
        self.capacity
            .store(IVC_RING_CAPACITY as u32, Ordering::Relaxed);
        self.slot_size
            .store(IVC_SLOT_SIZE as u32, Ordering::Relaxed);
        self.head.store(0, Ordering::Relaxed);
        for slot in &self.slots {
            slot.clear();
        }
        self.tail.store(0, Ordering::Release);
    }

    pub(crate) fn layout_matches(&self, direction: IvcRingDirection) -> bool {
        self.direction.load(Ordering::Relaxed) == direction as u32
            && self.capacity.load(Ordering::Relaxed) == IVC_RING_CAPACITY as u32
            && self.slot_size.load(Ordering::Relaxed) == IVC_SLOT_SIZE as u32
    }

    pub(crate) fn available_slots(&self) -> usize {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        IVC_RING_CAPACITY.saturating_sub(tail.wrapping_sub(head) as usize)
    }

    pub(crate) fn has_pending_slots(&self) -> bool {
        self.head.load(Ordering::Relaxed) != self.tail.load(Ordering::Acquire)
    }

    pub(crate) fn try_push_slot(&self, slot: &[u8; IVC_SLOT_SIZE]) -> Result<(), IvcSlotError> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) as usize >= IVC_RING_CAPACITY {
            return Err(IvcSlotError::Full);
        }

        let slot_index = tail as usize % IVC_RING_CAPACITY;
        self.slots[slot_index].write(slot);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    pub(crate) fn try_peek_slot(&self, output: &mut [u8; IVC_SLOT_SIZE]) -> bool {
        self.try_peek_slot_at(0, output)
    }

    pub(crate) fn try_peek_slot_at(&self, offset: usize, output: &mut [u8; IVC_SLOT_SIZE]) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if offset >= IVC_RING_CAPACITY || offset >= tail.wrapping_sub(head) as usize {
            return false;
        }

        // The sole consumer does not release head while inspecting this
        // window, so the producer cannot overwrite any acquired slot in it.
        let slot_index = head.wrapping_add(offset as u32) as usize % IVC_RING_CAPACITY;
        self.slots[slot_index].read(output);
        true
    }

    pub(crate) fn pop_slot(&self) {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        debug_assert_ne!(head, tail, "a slot must be peeked before it is popped");
        self.head.store(head.wrapping_add(1), Ordering::Release);
    }
}

/// One fixed-size opaque ring slot.
#[repr(C, align(256))]
struct IvcSlot {
    bytes: UnsafeCell<[u8; IVC_SLOT_SIZE]>,
}

impl IvcSlot {
    fn clear(&self) {
        // SAFETY: Initialization occurs before the region is published to a
        // peer, so no endpoint can access these valid, aligned slot bytes.
        unsafe { self.bytes.get().write([0; IVC_SLOT_SIZE]) };
    }

    fn write(&self, slot: &[u8; IVC_SLOT_SIZE]) {
        // SAFETY: The sole producer owns this in-bounds slot until `tail`
        // publishes it; acquiring `head` ensures the previous read is done.
        unsafe { self.bytes.get().write(*slot) };
    }

    fn read(&self, output: &mut [u8; IVC_SLOT_SIZE]) {
        // SAFETY: Acquiring `tail` makes this initialized slot visible to the
        // sole consumer; `head` is released only after copying all its bytes.
        unsafe { output.copy_from_slice(&*self.bytes.get()) };
    }
}

#[cfg(test)]
pub(crate) fn new_ring_for_test() -> IvcRing {
    IvcRing {
        direction: AtomicU32::new(0),
        capacity: AtomicU32::new(0),
        slot_size: AtomicU32::new(0),
        head: AtomicU32::new(0),
        tail: AtomicU32::new(0),
        reserved: [const { AtomicU32::new(0) }; 3],
        slots: [const {
            IvcSlot {
                bytes: UnsafeCell::new([0; IVC_SLOT_SIZE]),
            }
        }; IVC_RING_CAPACITY],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opaque_slots_are_fifo_and_full_slots_are_not_overwritten() {
        // These offsets and sizes are part of the shared-memory ABI, not
        // implementation details. A padding change would corrupt peer traffic.
        assert_eq!(core::mem::size_of::<IvcRing>(), 8448);
        assert_eq!(core::mem::align_of::<IvcRing>(), 256);
        assert_eq!(core::mem::size_of::<IvcSlot>(), 256);
        assert_eq!(core::mem::offset_of!(IvcRing, direction), 0);
        assert_eq!(core::mem::offset_of!(IvcRing, capacity), 4);
        assert_eq!(core::mem::offset_of!(IvcRing, slot_size), 8);
        assert_eq!(core::mem::offset_of!(IvcRing, head), 12);
        assert_eq!(core::mem::offset_of!(IvcRing, tail), 16);
        assert_eq!(core::mem::offset_of!(IvcRing, slots), 256);

        let ring = new_ring_for_test();
        ring.initialize(IvcRingDirection::PublisherToSubscriber);
        for (field, expected, incompatible) in [
            (&ring.direction, 1, 2),
            (&ring.capacity, 32, 16),
            (&ring.slot_size, 256, 64),
        ] {
            assert_eq!(field.load(Ordering::Relaxed), expected);
            field.store(incompatible, Ordering::Relaxed);
            assert!(!ring.layout_matches(IvcRingDirection::PublisherToSubscriber));
            field.store(expected, Ordering::Relaxed);
        }
        assert!(ring.layout_matches(IvcRingDirection::PublisherToSubscriber));

        for value in 0..IVC_RING_CAPACITY {
            ring.try_push_slot(&[value as u8; IVC_SLOT_SIZE]).unwrap();
        }
        assert_eq!(
            ring.try_push_slot(&[0xff; IVC_SLOT_SIZE]),
            Err(IvcSlotError::Full)
        );

        for value in 0..IVC_RING_CAPACITY {
            let mut slot = [0u8; IVC_SLOT_SIZE];
            assert!(ring.try_peek_slot(&mut slot));
            assert_eq!(slot, [value as u8; IVC_SLOT_SIZE]);
            ring.pop_slot();
        }
        let mut slot = [0u8; IVC_SLOT_SIZE];
        assert!(!ring.try_peek_slot(&mut slot));
    }
}
