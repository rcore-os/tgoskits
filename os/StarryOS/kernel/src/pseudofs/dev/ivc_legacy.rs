//! Starry-local compatibility with the fixed-slot IVC v2 guest protocol.
//!
//! Derived from `virtualization/axivc/src/{region,ring,endpoint,message}.rs` at
//! `714accd8f636c540b2c3554b0b1e5cb885be42a4`. Keep the wire layout and SPSC
//! ownership contract compatible with existing v2 guests, independently of the
//! workspace Message V1 implementation. Remove this module when Starry's legacy
//! IVC devices are replaced by ivshmem; do not add new transport features here.

use core::{
    cell::UnsafeCell,
    sync::atomic::{AtomicU32, AtomicU64, Ordering},
};

const IVC_REGION_MAGIC: u32 = 0x4956_4332;
const IVC_REGION_VERSION: u16 = 2;
const IVC_REGION_FEATURE_SPSC_FIXED_SLOTS: u32 = 1;
const IVC_RING_CAPACITY: usize = 16;
pub(super) const IVC_SLOT_PAYLOAD_SIZE: usize = 48;

const IVC_REGION_HEADER_SIZE: u32 = core::mem::size_of::<IvcRegionHeader>() as u32;
const IVC_REGION_TOTAL_SIZE: u32 = core::mem::size_of::<IvcRegion>() as u32;
const IVC_PUBLISHER_TO_SUBSCRIBER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, publisher_to_subscriber) as u32;
const IVC_SUBSCRIBER_TO_PUBLISHER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, subscriber_to_publisher) as u32;
const IVC_RING_HEADER_SIZE: u32 = core::mem::size_of::<IvcRing>() as u32;

/// Full fixed-slot IVC region for one publisher/subscriber pair.
///
/// Axvisor enforces at most one subscriber for the SPSC protocol. The leading
/// publisher ID and channel key are initialized by Axvisor; the remaining
/// fields are owned by the guest protocol.
#[repr(C, align(64))]
pub(super) struct IvcRegion {
    publisher_id: u64,
    key: u64,
    header: IvcRegionHeader,
    publisher_to_subscriber: IvcRing,
    subscriber_to_publisher: IvcRing,
}

// SAFETY: Each ring has one producer and one consumer. The unsafe attachment
// methods require unique endpoints per role, and endpoint operations require
// mutable access. Header fields are initialized before sharing or are atomic.
unsafe impl Sync for IvcRegion {}

impl IvcRegion {
    /// Initializes guest-owned state without rewriting the host channel header.
    pub(super) fn initialize(&mut self) {
        self.publisher_to_subscriber
            .initialize(IvcRingDirection::PublisherToSubscriber);
        self.subscriber_to_publisher
            .initialize(IvcRingDirection::SubscriberToPublisher);
        // Publish the header after both rings are ready. Acquiring magic lets
        // a peer use the initialized rings immediately.
        self.header.initialize();
    }

    pub(super) fn channel_header_matches(&self, publisher_id: usize, key: usize) -> bool {
        self.publisher_id == publisher_id as u64 && self.key == key as u64
    }

    pub(super) fn publisher_id(&self) -> usize {
        self.publisher_id as usize
    }

    pub(super) fn protocol_header_matches(&self) -> bool {
        self.header.magic.load(Ordering::Acquire) == IVC_REGION_MAGIC
            && self.header.version.load(Ordering::Acquire) == IVC_REGION_VERSION
            && self.header.region_size.load(Ordering::Acquire) as usize
                >= core::mem::size_of::<Self>()
    }

    /// Attaches the publisher's request producer and acknowledgement consumer.
    ///
    /// # Safety
    ///
    /// The publisher role must be attached only once across all address spaces
    /// sharing this region. Duplicate endpoints could race on slot payloads.
    pub(super) unsafe fn publisher_endpoints(&self) -> IvcEndpoints<'_> {
        IvcEndpoints::new(&self.publisher_to_subscriber, &self.subscriber_to_publisher)
    }

    /// Attaches the subscriber's acknowledgement producer and request consumer.
    ///
    /// # Safety
    ///
    /// The subscriber role must be attached only once across all address spaces
    /// sharing this region. Duplicate endpoints could race on slot payloads.
    pub(super) unsafe fn subscriber_endpoints(&self) -> IvcEndpoints<'_> {
        IvcEndpoints::new(&self.subscriber_to_publisher, &self.publisher_to_subscriber)
    }
}

/// Protocol metadata shared by guests, retaining the v2 field widths.
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
            .store(IVC_REGION_HEADER_SIZE as u16, Ordering::Relaxed);
        self.region_size
            .store(IVC_REGION_TOTAL_SIZE, Ordering::Relaxed);
        self.features
            .store(IVC_REGION_FEATURE_SPSC_FIXED_SLOTS, Ordering::Relaxed);
        self.publisher_to_subscriber_offset
            .store(IVC_PUBLISHER_TO_SUBSCRIBER_RING_OFFSET, Ordering::Relaxed);
        self.subscriber_to_publisher_offset
            .store(IVC_SUBSCRIBER_TO_PUBLISHER_RING_OFFSET, Ordering::Relaxed);
        self.ring_size
            .store(IVC_RING_HEADER_SIZE, Ordering::Relaxed);
        self.version.store(IVC_REGION_VERSION, Ordering::Release);
        self.magic.store(IVC_REGION_MAGIC, Ordering::Release);
    }
}

/// V2 stores each nominal u16 header field in a separate aligned u32 word.
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

/// Unique producer and consumer belonging to one channel role.
pub(super) struct IvcEndpoints<'a> {
    producer: IvcProducer<'a>,
    consumer: IvcConsumer<'a>,
}

impl<'a> IvcEndpoints<'a> {
    const fn new(producer: &'a IvcRing, consumer: &'a IvcRing) -> Self {
        Self {
            producer: IvcProducer { ring: producer },
            consumer: IvcConsumer { ring: consumer },
        }
    }

    pub(super) fn into_parts(self) -> (IvcProducer<'a>, IvcConsumer<'a>) {
        (self.producer, self.consumer)
    }
}

/// Non-cloneable sending endpoint; publication requires exclusive access.
pub(super) struct IvcProducer<'a> {
    ring: &'a IvcRing,
}

impl IvcProducer<'_> {
    pub(super) fn send(
        &mut self,
        kind: IvcMessageKind,
        sequence: u64,
        payload: &[u8],
    ) -> Result<(), IvcRingError> {
        self.ring.send(kind, sequence, payload)
    }

    pub(super) fn can_send(&self) -> bool {
        self.ring.can_send()
    }
}

/// Non-cloneable receiving endpoint; consumption requires exclusive access.
pub(super) struct IvcConsumer<'a> {
    ring: &'a IvcRing,
}

impl IvcConsumer<'_> {
    /// Copies the oldest message, retaining the slot when validation fails or
    /// the output buffer is too short.
    pub(super) fn try_recv(
        &mut self,
        payload: &mut [u8],
    ) -> Result<Option<IvcMessage>, IvcRingError> {
        self.ring.try_recv(payload)
    }

    pub(super) fn can_recv(&self) -> bool {
        self.ring.can_recv()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u16)]
pub(super) enum IvcMessageKind {
    Request = 1,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct IvcMessage {
    sequence: u64,
    kind: IvcMessageKind,
    len: usize,
}

impl IvcMessage {
    pub(super) const fn sequence(self) -> u64 {
        self.sequence
    }

    pub(super) const fn kind(self) -> IvcMessageKind {
        self.kind
    }

    pub(super) const fn len(self) -> usize {
        self.len
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum IvcRingError {
    Full,
    PayloadTooLarge { len: usize, capacity: usize },
    BufferTooSmall { required: usize, available: usize },
    UnknownMessageKind(u16),
}

#[derive(Clone, Copy)]
#[repr(u16)]
enum IvcRingDirection {
    PublisherToSubscriber = 1,
    SubscriberToPublisher = 2,
}

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

// SAFETY: Role attachment guarantees one producer and one consumer. The
// producer writes before releasing tail; the consumer acquires tail before
// reading and releases head only after copying. The producer acquires head
// before reusing a slot, so payload accesses do not race.
unsafe impl Sync for IvcRing {}

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
        if payload.len() > IVC_SLOT_PAYLOAD_SIZE {
            return Err(IvcRingError::PayloadTooLarge {
                len: payload.len(),
                capacity: IVC_SLOT_PAYLOAD_SIZE,
            });
        }

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

    fn can_send(&self) -> bool {
        let tail = self.tail.load(Ordering::Relaxed);
        let head = self.head.load(Ordering::Acquire);
        (tail.wrapping_sub(head) as usize) < IVC_RING_CAPACITY
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

    fn can_recv(&self) -> bool {
        let head = self.head.load(Ordering::Relaxed);
        let tail = self.tail.load(Ordering::Acquire);
        head != tail
    }
}

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
        let len = payload.len();
        // SAFETY: send checked that len fits this slot. The sole producer owns
        // it until tail is released; acquiring head excludes a prior reader.
        // The source slice cannot alias the private UnsafeCell payload.
        unsafe {
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
        let len = self.len.load(Ordering::Relaxed) as usize;
        if len > IVC_SLOT_PAYLOAD_SIZE {
            return Err(IvcRingError::PayloadTooLarge {
                len,
                capacity: IVC_SLOT_PAYLOAD_SIZE,
            });
        }
        if payload.len() < len {
            return Err(IvcRingError::BufferTooSmall {
                required: len,
                available: payload.len(),
            });
        }
        // SAFETY: Acquiring tail exposed an initialized slot. The checked len
        // fits both buffers, the output cannot alias the private slot payload,
        // and head is released only after copying, preventing producer reuse.
        unsafe {
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

#[cfg(all(test, not(axtest)))]
mod tests {
    extern crate std;

    use core::mem::{align_of, offset_of, size_of};

    use super::*;

    fn new_region() -> IvcRegion {
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
        IvcRegion {
            publisher_id: 1,
            key: 0x4956_4301,
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

    #[test]
    fn initialization_retains_the_v2_wire_contract_and_rejects_v3() {
        // These are peer-visible v2 ABI values, not Message V1 constants.
        assert_eq!(size_of::<IvcRegion>(), 2240);
        assert_eq!(align_of::<IvcRegion>(), 64);
        assert_eq!(offset_of!(IvcRegion, header), 16);
        assert_eq!(offset_of!(IvcRegion, publisher_to_subscriber), 64);
        assert_eq!(offset_of!(IvcRegion, subscriber_to_publisher), 1152);
        assert_eq!(size_of::<IvcRing>(), 1088);
        assert_eq!(offset_of!(IvcRing, head), 12);
        assert_eq!(offset_of!(IvcRing, tail), 16);
        assert_eq!(offset_of!(IvcRing, slots), 64);
        assert_eq!(size_of::<IvcMessageSlot>(), 64);
        assert_eq!(offset_of!(IvcMessageSlot, len), 8);
        assert_eq!(offset_of!(IvcMessageSlot, kind), 12);
        assert_eq!(offset_of!(IvcMessageSlot, payload), 16);

        let mut region = new_region();
        assert!(!region.protocol_header_matches());
        region.initialize();
        assert!(region.channel_header_matches(1, 0x4956_4301));
        assert!(!region.channel_header_matches(2, 0x4956_4301));
        assert!(region.protocol_header_matches());
        let header = &region.header;
        assert_eq!(header.magic.load(Ordering::Acquire), 0x4956_4332);
        assert_eq!(header.version.load(Ordering::Relaxed), 2);
        assert_eq!(header.header_size.load(Ordering::Relaxed), 32);
        assert_eq!(header.region_size.load(Ordering::Relaxed), 2240);
        assert_eq!(header.features.load(Ordering::Relaxed), 1);
        assert_eq!(
            header
                .publisher_to_subscriber_offset
                .load(Ordering::Relaxed),
            64
        );
        assert_eq!(
            header
                .subscriber_to_publisher_offset
                .load(Ordering::Relaxed),
            1152
        );
        assert_eq!(header.ring_size.load(Ordering::Relaxed), 1088);
        header.version.store(3, Ordering::Release);
        assert!(!region.protocol_header_matches());
    }

    #[test]
    fn duplex_fifo_and_readiness_survive_full_rings_and_counter_wraparound() {
        let mut region = new_region();
        region.initialize();
        // Start near u32 overflow as well as crossing the physical ring end.
        let ring = &region.publisher_to_subscriber;
        ring.head.store(u32::MAX - 7, Ordering::Relaxed);
        ring.tail.store(u32::MAX - 7, Ordering::Relaxed);
        // SAFETY: each role is attached once for this region.
        let (mut tx, mut ack_rx) = unsafe { region.publisher_endpoints() }.into_parts();
        // SAFETY: each role is attached once for this region.
        let (mut ack_tx, mut rx) = unsafe { region.subscriber_endpoints() }.into_parts();

        let mut output = [0; IVC_SLOT_PAYLOAD_SIZE];
        for cycle in 0..4 {
            assert!(tx.can_send());
            assert!(!rx.can_recv());
            for slot in 0..IVC_RING_CAPACITY {
                let sequence = (cycle * IVC_RING_CAPACITY + slot) as u64;
                tx.send(IvcMessageKind::Request, sequence, &[sequence as u8; 48])
                    .unwrap();
            }
            assert!(!tx.can_send());
            assert!(rx.can_recv());
            assert_eq!(
                tx.send(IvcMessageKind::Request, 100, b"full"),
                Err(IvcRingError::Full)
            );
            // The reverse direction must remain usable while requests are full.
            ack_tx
                .send(IvcMessageKind::Ack, cycle as u64, b"ack")
                .unwrap();
            let ack = ack_rx.try_recv(&mut output).unwrap().unwrap();
            assert_eq!(ack.kind(), IvcMessageKind::Ack);
            assert_eq!(ack.sequence(), cycle as u64);
            assert_eq!(&output[..ack.len()], b"ack");
            for slot in 0..IVC_RING_CAPACITY {
                let sequence = (cycle * IVC_RING_CAPACITY + slot) as u64;
                let message = rx.try_recv(&mut output).unwrap().unwrap();
                assert_eq!(message.kind(), IvcMessageKind::Request);
                assert_eq!(message.sequence(), sequence);
                assert_eq!(message.len(), 48);
                assert_eq!(output, [sequence as u8; 48]);
            }
            assert!(tx.can_send());
            assert_eq!(rx.try_recv(&mut output), Ok(None));
            assert!(region.protocol_header_matches());
            assert!(region.channel_header_matches(1, 0x4956_4301));
        }
    }

    #[test]
    fn invalid_payloads_and_short_buffers_preserve_the_pending_message() {
        let mut region = new_region();
        region.initialize();
        // SAFETY: each role is attached once for this region.
        let (mut tx, _) = unsafe { region.publisher_endpoints() }.into_parts();
        // SAFETY: each role is attached once for this region.
        let (_, mut rx) = unsafe { region.subscriber_endpoints() }.into_parts();
        assert_eq!(
            tx.send(IvcMessageKind::Request, 1, &[0; 49]),
            Err(IvcRingError::PayloadTooLarge {
                len: 49,
                capacity: 48
            })
        );
        let mut output = [0; 48];
        assert_eq!(rx.try_recv(&mut output), Ok(None));
        tx.send(IvcMessageKind::Request, 7, b"payload").unwrap();
        assert_eq!(
            rx.try_recv(&mut [0; 3]),
            Err(IvcRingError::BufferTooSmall {
                required: 7,
                available: 3
            })
        );
        let slot = &region.publisher_to_subscriber.slots[0];
        // Simulate malformed peer metadata without any concurrent writer.
        slot.kind.store(99, Ordering::Relaxed);
        assert_eq!(
            rx.try_recv(&mut output),
            Err(IvcRingError::UnknownMessageKind(99))
        );
        slot.kind.store(1, Ordering::Relaxed);
        slot.len.store(49, Ordering::Relaxed);
        assert_eq!(
            rx.try_recv(&mut output),
            Err(IvcRingError::PayloadTooLarge {
                len: 49,
                capacity: 48
            })
        );
        slot.len.store(7, Ordering::Relaxed);
        let message = rx.try_recv(&mut output).unwrap().unwrap();
        assert_eq!(message.sequence(), 7);
        assert_eq!(&output[..message.len()], b"payload");
        assert_eq!(rx.try_recv(&mut output), Ok(None));
        tx.send(IvcMessageKind::Request, 8, b"").unwrap();
        let empty = rx.try_recv(&mut []).unwrap().unwrap();
        assert_eq!(empty.sequence(), 8);
        assert_eq!(empty.len(), 0);
    }

    #[test]
    fn spsc_endpoints_deliver_intact_messages_across_threads() {
        let mut region = new_region();
        region.initialize();
        // SAFETY: each role is attached once; scoped threads cannot outlive it.
        let (mut tx, _) = unsafe { region.publisher_endpoints() }.into_parts();
        // SAFETY: each role is attached once; scoped threads cannot outlive it.
        let (_, mut rx) = unsafe { region.subscriber_endpoints() }.into_parts();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        std::thread::scope(|scope| {
            scope.spawn(move || {
                for sequence in 0..1024 {
                    let payload = [sequence as u8; 48];
                    loop {
                        match tx.send(IvcMessageKind::Request, sequence, &payload) {
                            Ok(()) => break,
                            Err(IvcRingError::Full) => {
                                assert!(std::time::Instant::now() < deadline, "send timed out");
                                std::thread::yield_now();
                            }
                            Err(error) => panic!("send failed: {error:?}"),
                        }
                    }
                }
            });
            let mut output = [0; 48];
            for sequence in 0..1024 {
                let message = loop {
                    if let Some(message) = rx.try_recv(&mut output).unwrap() {
                        break message;
                    }
                    assert!(std::time::Instant::now() < deadline, "receive timed out");
                    std::thread::yield_now();
                };
                assert_eq!(message.sequence(), sequence);
                assert_eq!(message.kind(), IvcMessageKind::Request);
                assert_eq!(message.len(), 48);
                assert_eq!(output, [sequence as u8; 48]);
            }
        });
    }
}
