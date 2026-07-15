use core::{
    cell::UnsafeCell,
    cmp,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

use crate::{
    IVC_RING_CAPACITY, IVC_SLOT_PAYLOAD_SIZE,
    message::{IvcMessage, IvcMessageKind},
};

/// Direction of a one-way IVC ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum IvcRingDirection {
    /// Messages sent by the channel publisher and received by the subscriber.
    PublisherToSubscriber = 1,
    /// Messages sent by the subscriber and received by the publisher.
    SubscriberToPublisher = 2,
}

/// Errors returned by the shared-memory ring protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IvcRingError {
    /// The ring has no free slot.
    Full,
    /// The stored message kind is not known by this protocol version.
    UnknownMessageKind(u16),
}

/// Single-producer, single-consumer fixed-slot ring.
#[repr(C, align(64))]
pub(crate) struct IvcRing {
    direction: AtomicU32,
    capacity: AtomicU32,
    slot_payload_size: AtomicU32,
    head: AtomicU32,
    tail: AtomicU32,
    reserved: [AtomicU32; 3],
    slots: [IvcMessageSlot; IVC_RING_CAPACITY],
}

impl IvcRing {
    pub(crate) fn initialize(&self, direction: IvcRingDirection) {
        self.direction.store(direction as u32, Ordering::Relaxed);
        self.capacity
            .store(IVC_RING_CAPACITY as u32, Ordering::Relaxed);
        self.slot_payload_size
            .store(IVC_SLOT_PAYLOAD_SIZE as u32, Ordering::Relaxed);
        self.head.store(0, Ordering::Relaxed);
        self.tail.store(0, Ordering::Release);
        for slot in &self.slots {
            slot.clear();
        }
    }

    pub(crate) fn send(
        &self,
        kind: IvcMessageKind,
        sequence: u64,
        payload: &[u8],
    ) -> Result<(), IvcRingError> {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        if tail.wrapping_sub(head) as usize >= IVC_RING_CAPACITY {
            return Err(IvcRingError::Full);
        }

        let slot_index = tail as usize % IVC_RING_CAPACITY;
        self.slots[slot_index].write(kind, sequence, payload);
        self.tail.store(tail.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    pub(crate) fn try_recv(&self, payload: &mut [u8]) -> Result<Option<IvcMessage>, IvcRingError> {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        if head == tail {
            return Ok(None);
        }

        let slot_index = head as usize % IVC_RING_CAPACITY;
        let message = self.slots[slot_index].read(payload)?;
        self.head.store(head.wrapping_add(1), Ordering::Release);
        Ok(Some(message))
    }
}

/// One fixed-size ring slot.
#[repr(C, align(64))]
pub(crate) struct IvcMessageSlot {
    sequence: AtomicU64,
    len: AtomicU32,
    kind: AtomicU32,
    payload: UnsafeCell<[u8; IVC_SLOT_PAYLOAD_SIZE]>,
}

impl IvcMessageSlot {
    fn clear(&self) {
        self.sequence.store(0, Ordering::Relaxed);
        self.len.store(0, Ordering::Relaxed);
        self.kind.store(0, Ordering::Relaxed);
    }

    fn write(&self, kind: IvcMessageKind, sequence: u64, payload: &[u8]) {
        let len = cmp::min(payload.len(), IVC_SLOT_PAYLOAD_SIZE);
        unsafe {
            // The slot is exclusively owned by the producer until tail is
            // released, so writing through this interior cell cannot race with
            // another writer for this SPSC ring.
            let target = self.payload.get().cast::<u8>();
            core::ptr::copy_nonoverlapping(payload.as_ptr(), target, len);
            if len < IVC_SLOT_PAYLOAD_SIZE {
                core::ptr::write_bytes(target.add(len), 0, IVC_SLOT_PAYLOAD_SIZE - len);
            }
        }
        self.sequence.store(sequence, Ordering::Relaxed);
        self.len.store(len as u32, Ordering::Relaxed);
        self.kind.store(kind as u32, Ordering::Relaxed);
    }

    fn read(&self, payload: &mut [u8]) -> Result<IvcMessage, IvcRingError> {
        let raw_kind = self.kind.load(Ordering::Relaxed) as u16;
        let Some(kind) = IvcMessageKind::from_raw(raw_kind) else {
            return Err(IvcRingError::UnknownMessageKind(raw_kind));
        };
        let sequence = self.sequence.load(Ordering::Relaxed);
        let len = cmp::min(
            self.len.load(Ordering::Relaxed) as usize,
            cmp::min(IVC_SLOT_PAYLOAD_SIZE, payload.len()),
        );
        unsafe {
            // The consumer observes this slot only after tail acquire. Payload
            // bytes are copied out before head release returns ownership.
            core::ptr::copy_nonoverlapping(
                self.payload.get().cast::<u8>(),
                payload.as_mut_ptr(),
                len,
            );
        }
        Ok(IvcMessage::new(sequence, kind, len))
    }
}

#[cfg(test)]
pub(crate) fn new_ring_for_test() -> IvcRing {
    IvcRing {
        direction: AtomicU32::new(0),
        capacity: AtomicU32::new(0),
        slot_payload_size: AtomicU32::new(0),
        head: AtomicU32::new(0),
        tail: AtomicU32::new(0),
        reserved: [const { AtomicU32::new(0) }; 3],
        slots: [const {
            IvcMessageSlot {
                sequence: AtomicU64::new(0),
                len: AtomicU32::new(0),
                kind: AtomicU32::new(0),
                payload: UnsafeCell::new([0; IVC_SLOT_PAYLOAD_SIZE]),
            }
        }; IVC_RING_CAPACITY],
    }
}
