#![no_std]

//! Shared-memory protocol helpers for AxVisor inter-VM communication.

use core::{
    cell::UnsafeCell,
    cmp,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

/// Magic value stored in `IvcRegionHeader`.
pub const IVC_REGION_MAGIC: u32 = 0x4956_4332;
/// Current shared-memory protocol version.
pub const IVC_REGION_VERSION: u16 = 2;
/// Fixed slot payload capacity.
pub const IVC_SLOT_PAYLOAD_SIZE: usize = 48;
/// Number of slots per one-way ring.
pub const IVC_RING_CAPACITY: usize = 16;

const RING_HEADER_SIZE: u32 = core::mem::size_of::<IvcRing>() as u32;
const PUBLISHER_TO_SUBSCRIBER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, publisher_to_subscriber) as u32;
const SUBSCRIBER_TO_PUBLISHER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, subscriber_to_publisher) as u32;
const IVC_REGION_FEATURE_SPSC_FIXED_SLOTS: u32 = 1;

/// Direction of a one-way IVC ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum IvcRingDirection {
    /// Messages sent by the channel publisher and received by the subscriber.
    PublisherToSubscriber = 1,
    /// Messages sent by the subscriber and received by the publisher.
    SubscriberToPublisher = 2,
}

/// Message class used by the fixed-slot IVC protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub enum IvcMessageKind {
    /// Publisher request payload.
    Request = 1,
    /// Subscriber acknowledgement payload.
    Ack     = 2,
}

impl IvcMessageKind {
    const fn from_raw(raw: u16) -> Option<Self> {
        match raw {
            1 => Some(Self::Request),
            2 => Some(Self::Ack),
            _ => None,
        }
    }
}

/// Errors returned by the shared-memory ring protocol.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IvcRingError {
    /// The ring has no free slot.
    Full,
    /// The stored message kind is not known by this protocol version.
    UnknownMessageKind(u16),
}

/// One message copied out of an IVC ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IvcMessage {
    sequence: u64,
    kind: IvcMessageKind,
    len: usize,
}

impl IvcMessage {
    /// Returns the message sequence number.
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Returns the message kind.
    pub const fn kind(self) -> IvcMessageKind {
        self.kind
    }

    /// Returns the copied payload length.
    pub const fn len(self) -> usize {
        self.len
    }

    /// Returns whether the copied payload is empty.
    pub const fn is_empty(self) -> bool {
        self.len == 0
    }
}

/// Full fixed-slot IVC region.
///
/// The first two fields intentionally match `axvm::runtime::ivc::IVCChannelHeader`.
/// Axvisor initializes them when the host-side channel is created. The remaining
/// fields are owned by this shared-memory protocol.
#[repr(C, align(64))]
pub struct IvcRegion {
    publisher_id: u64,
    key: u64,
    header: IvcRegionHeader,
    publisher_to_subscriber: IvcRing,
    subscriber_to_publisher: IvcRing,
}

impl IvcRegion {
    /// Initializes the protocol region and preserves the Axvisor IVC header.
    pub fn initialize(&mut self, publisher_id: usize, key: usize) {
        self.publisher_id = publisher_id as u64;
        self.key = key as u64;
        self.header.initialize();
        self.publisher_to_subscriber
            .initialize(IvcRingDirection::PublisherToSubscriber);
        self.subscriber_to_publisher
            .initialize(IvcRingDirection::SubscriberToPublisher);
    }

    /// Returns whether the host-provided IVC channel header matches.
    pub fn channel_header_matches(&self, publisher_id: usize, key: usize) -> bool {
        self.publisher_id == publisher_id as u64 && self.key == key as u64
    }

    /// Returns whether the protocol header is supported by this crate.
    pub fn protocol_header_matches(&self) -> bool {
        self.header.magic.load(Ordering::Acquire) == IVC_REGION_MAGIC
            && self.header.version.load(Ordering::Acquire) == IVC_REGION_VERSION
            && self.header.region_size.load(Ordering::Acquire) as usize
                >= core::mem::size_of::<Self>()
    }

    /// Sends one publisher-to-subscriber message.
    pub fn send_request(&self, sequence: u64, payload: &[u8]) -> Result<(), IvcRingError> {
        self.publisher_to_subscriber
            .send(IvcMessageKind::Request, sequence, payload)
    }

    /// Receives one publisher-to-subscriber message.
    pub fn try_recv_request(&self, payload: &mut [u8]) -> Result<Option<IvcMessage>, IvcRingError> {
        self.publisher_to_subscriber.try_recv(payload)
    }

    /// Sends one subscriber-to-publisher acknowledgement.
    pub fn send_ack(&self, sequence: u64, payload: &[u8]) -> Result<(), IvcRingError> {
        self.subscriber_to_publisher
            .send(IvcMessageKind::Ack, sequence, payload)
    }

    /// Receives one subscriber-to-publisher acknowledgement.
    pub fn try_recv_ack(&self, payload: &mut [u8]) -> Result<Option<IvcMessage>, IvcRingError> {
        self.subscriber_to_publisher.try_recv(payload)
    }
}

/// Protocol metadata shared by guests.
#[repr(C, align(8))]
struct IvcRegionHeader {
    magic: AtomicU32,
    version: AtomicU16Compat,
    header_size: AtomicU16Compat,
    region_size: AtomicU32,
    features: AtomicU32,
    publisher_to_subscriber_offset: AtomicU32,
    subscriber_to_publisher_offset: AtomicU32,
    ring_size: AtomicU32,
}

impl IvcRegionHeader {
    fn initialize(&self) {
        self.header_size
            .store(core::mem::size_of::<Self>() as u16, Ordering::Relaxed);
        self.region_size
            .store(core::mem::size_of::<IvcRegion>() as u32, Ordering::Relaxed);
        self.features
            .store(IVC_REGION_FEATURE_SPSC_FIXED_SLOTS, Ordering::Relaxed);
        self.publisher_to_subscriber_offset
            .store(PUBLISHER_TO_SUBSCRIBER_RING_OFFSET, Ordering::Relaxed);
        self.subscriber_to_publisher_offset
            .store(SUBSCRIBER_TO_PUBLISHER_RING_OFFSET, Ordering::Relaxed);
        self.ring_size.store(RING_HEADER_SIZE, Ordering::Relaxed);
        self.version.store(IVC_REGION_VERSION, Ordering::Release);
        self.magic.store(IVC_REGION_MAGIC, Ordering::Release);
    }
}

/// Atomic `u16` stored as an aligned `AtomicU32` for portability.
#[repr(transparent)]
struct AtomicU16Compat(AtomicU32);

impl AtomicU16Compat {
    fn load(&self, ordering: Ordering) -> u16 {
        self.0.load(ordering) as u16
    }

    fn store(&self, value: u16, ordering: Ordering) {
        self.0.store(value as u32, ordering);
    }
}

/// Single-producer, single-consumer fixed-slot ring.
#[repr(C, align(64))]
struct IvcRing {
    direction: AtomicU32,
    capacity: AtomicU32,
    slot_payload_size: AtomicU32,
    head: AtomicU32,
    tail: AtomicU32,
    reserved: [AtomicU32; 3],
    slots: [IvcMessageSlot; IVC_RING_CAPACITY],
}

impl IvcRing {
    fn initialize(&self, direction: IvcRingDirection) {
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

    fn send(
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

    fn try_recv(&self, payload: &mut [u8]) -> Result<Option<IvcMessage>, IvcRingError> {
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
struct IvcMessageSlot {
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
        Ok(IvcMessage {
            sequence,
            kind,
            len,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANNEL_KEY: usize = 0x4956_4301;
    const PUBLISHER_VM_ID: usize = 1;
    const CHANNEL_SIZE: usize = 4096;

    #[test]
    fn region_header_and_channel_header_match_after_initialize() {
        let mut region = new_region();

        region.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

        assert!(region.channel_header_matches(PUBLISHER_VM_ID, CHANNEL_KEY));
        assert!(region.protocol_header_matches());
    }

    #[test]
    fn request_ring_delivers_messages_in_fifo_order() {
        let mut region = new_region();
        region.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

        region.send_request(1, b"one").unwrap();
        region.send_request(2, b"two").unwrap();

        let mut payload = [0; IVC_SLOT_PAYLOAD_SIZE];
        let first = region.try_recv_request(&mut payload).unwrap().unwrap();
        assert_eq!(first.kind(), IvcMessageKind::Request);
        assert_eq!(first.sequence(), 1);
        assert_eq!(&payload[..first.len()], b"one");

        let second = region.try_recv_request(&mut payload).unwrap().unwrap();
        assert_eq!(second.sequence(), 2);
        assert_eq!(&payload[..second.len()], b"two");
        assert_eq!(region.try_recv_request(&mut payload), Ok(None));
    }

    #[test]
    fn ack_ring_is_independent_from_request_ring() {
        let mut region = new_region();
        region.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

        region.send_request(9, b"request").unwrap();
        region.send_ack(9, b"ack").unwrap();

        let mut payload = [0; IVC_SLOT_PAYLOAD_SIZE];
        let ack = region.try_recv_ack(&mut payload).unwrap().unwrap();
        assert_eq!(ack.kind(), IvcMessageKind::Ack);
        assert_eq!(ack.sequence(), 9);
        assert_eq!(&payload[..ack.len()], b"ack");

        let request = region.try_recv_request(&mut payload).unwrap().unwrap();
        assert_eq!(request.kind(), IvcMessageKind::Request);
        assert_eq!(request.sequence(), 9);
    }

    #[test]
    fn send_fails_when_ring_is_full() {
        let mut region = new_region();
        region.initialize(PUBLISHER_VM_ID, CHANNEL_KEY);

        for sequence in 0..IVC_RING_CAPACITY as u64 {
            region.send_request(sequence, b"x").unwrap();
        }

        assert_eq!(
            region.send_request(IVC_RING_CAPACITY as u64, b"x"),
            Err(IvcRingError::Full)
        );
    }

    #[test]
    fn protocol_region_fits_one_ivc_page() {
        assert!(core::mem::size_of::<IvcRegion>() <= CHANNEL_SIZE);
    }

    fn new_region() -> IvcRegion {
        IvcRegion {
            publisher_id: 0,
            key: 0,
            header: IvcRegionHeader {
                magic: AtomicU32::new(0),
                version: AtomicU16Compat(AtomicU32::new(0)),
                header_size: AtomicU16Compat(AtomicU32::new(0)),
                region_size: AtomicU32::new(0),
                features: AtomicU32::new(0),
                publisher_to_subscriber_offset: AtomicU32::new(0),
                subscriber_to_publisher_offset: AtomicU32::new(0),
                ring_size: AtomicU32::new(0),
            },
            publisher_to_subscriber: new_ring(),
            subscriber_to_publisher: new_ring(),
        }
    }

    fn new_ring() -> IvcRing {
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
}
