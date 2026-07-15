use core::sync::atomic::{AtomicU32, Ordering};

use crate::{
    IVC_REGION_MAGIC, IVC_REGION_VERSION,
    message::{IvcMessage, IvcMessageKind},
    ring::{IvcRing, IvcRingDirection, IvcRingError},
};

const RING_HEADER_SIZE: u32 = core::mem::size_of::<IvcRing>() as u32;
const PUBLISHER_TO_SUBSCRIBER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, publisher_to_subscriber) as u32;
const SUBSCRIBER_TO_PUBLISHER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, subscriber_to_publisher) as u32;
const IVC_REGION_FEATURE_SPSC_FIXED_SLOTS: u32 = 1;

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

#[cfg(test)]
pub(crate) fn new_region_for_test() -> IvcRegion {
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
        publisher_to_subscriber: crate::ring::new_ring_for_test(),
        subscriber_to_publisher: crate::ring::new_ring_for_test(),
    }
}
