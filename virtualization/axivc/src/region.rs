use core::sync::atomic::{AtomicU32, Ordering};

use crate::{
    IVC_REGION_MAGIC, IVC_REGION_VERSION,
    endpoint::IvcEndpoints,
    ring::{IvcRing, IvcRingDirection},
};

const RING_HEADER_SIZE: u32 = core::mem::size_of::<IvcRing>() as u32;
const PUBLISHER_TO_SUBSCRIBER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, publisher_to_subscriber) as u32;
const SUBSCRIBER_TO_PUBLISHER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, subscriber_to_publisher) as u32;
const IVC_REGION_FEATURE_SPSC_OPAQUE_CELLS: u32 = 1;

/// Full opaque-cell IVC region for one publisher/subscriber pair.
///
/// Axvisor enforces at most one subscriber for the current SPSC protocol. The
/// first two fields intentionally match `axvm::runtime::ivc::IVCChannelHeader`.
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

// SAFETY: The two rings are independent SPSC rings. Axvisor admits only one
// subscriber per channel, mutable ring state is reachable only through
// message sender/receiver endpoints with `&mut` methods, and the `unsafe`
// endpoint constructors require callers to keep one endpoint per ring role.
// The header fields are initialized once before sharing or are atomic, so
// concurrent &IvcRegion access across threads is sound.
unsafe impl Sync for IvcRegion {}

impl IvcRegion {
    /// Initializes the guest-owned protocol region.
    ///
    /// The leading channel header is initialized by Axvisor before the channel
    /// is made subscribable. It is read-only to guests, so protocol setup must
    /// not rewrite it while a subscriber may already be reading it.
    pub fn initialize(&mut self) {
        // A newly mapped GPA can contain bytes from an earlier session. Clear
        // publication first so a racing subscriber cannot accept stale layout
        // metadata while the rings are being reinitialized.
        self.header.invalidate();
        self.publisher_to_subscriber
            .initialize(IvcRingDirection::PublisherToSubscriber);
        self.subscriber_to_publisher
            .initialize(IvcRingDirection::SubscriberToPublisher);
        // Publish the protocol header only after both rings are ready. A peer
        // that observes `magic` with Acquire may immediately use the rings.
        self.header.initialize();
    }

    /// Returns whether the host-provided IVC channel header matches.
    pub fn channel_header_matches(&self, publisher_id: usize, key: usize) -> bool {
        self.publisher_id == publisher_id as u64 && self.key == key as u64
    }

    /// Returns whether the protocol header is supported by this crate.
    pub fn protocol_header_matches(&self) -> bool {
        self.header.matches()
            && self
                .publisher_to_subscriber
                .layout_matches(IvcRingDirection::PublisherToSubscriber)
            && self
                .subscriber_to_publisher
                .layout_matches(IvcRingDirection::SubscriberToPublisher)
    }

    /// Attaches the publisher side and returns its channel endpoints.
    ///
    /// The producer sends on the publisher-to-subscriber ring. The consumer
    /// receives from the subscriber-to-publisher ring.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the publisher role is attached only
    /// once across every address space sharing this region. Attaching it again
    /// would create duplicate producer and consumer endpoints, allowing data
    /// races on cell bytes.
    pub unsafe fn publisher_endpoints(&self) -> IvcEndpoints<'_> {
        IvcEndpoints::new(&self.publisher_to_subscriber, &self.subscriber_to_publisher)
    }

    /// Attaches the subscriber side and returns its channel endpoints.
    ///
    /// The producer sends on the subscriber-to-publisher ring. The consumer
    /// receives from the publisher-to-subscriber ring.
    ///
    /// # Safety
    ///
    /// The caller must guarantee that the subscriber role is attached only
    /// once across every address space sharing this region. Attaching it again
    /// would create duplicate producer and consumer endpoints, allowing data
    /// races on cell bytes.
    pub unsafe fn subscriber_endpoints(&self) -> IvcEndpoints<'_> {
        IvcEndpoints::new(&self.subscriber_to_publisher, &self.publisher_to_subscriber)
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
    fn matches(&self) -> bool {
        self.magic.load(Ordering::Acquire) == IVC_REGION_MAGIC
            && self.version.load(Ordering::Relaxed) == IVC_REGION_VERSION
            && self.header_size.load(Ordering::Relaxed) as usize == core::mem::size_of::<Self>()
            && self.region_size.load(Ordering::Relaxed) as usize
                >= core::mem::size_of::<IvcRegion>()
            && self.features.load(Ordering::Relaxed) == IVC_REGION_FEATURE_SPSC_OPAQUE_CELLS
            && self.publisher_to_subscriber_offset.load(Ordering::Relaxed)
                == PUBLISHER_TO_SUBSCRIBER_RING_OFFSET
            && self.subscriber_to_publisher_offset.load(Ordering::Relaxed)
                == SUBSCRIBER_TO_PUBLISHER_RING_OFFSET
            && self.ring_size.load(Ordering::Relaxed) == RING_HEADER_SIZE
    }

    fn invalidate(&self) {
        self.magic.store(0, Ordering::Release);
    }

    fn initialize(&self) {
        self.header_size
            .store(core::mem::size_of::<Self>() as u16, Ordering::Relaxed);
        self.region_size
            .store(core::mem::size_of::<IvcRegion>() as u32, Ordering::Relaxed);
        self.features
            .store(IVC_REGION_FEATURE_SPSC_OPAQUE_CELLS, Ordering::Relaxed);
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
pub(crate) fn new_region_for_test(publisher_id: usize, key: usize) -> IvcRegion {
    IvcRegion {
        publisher_id: publisher_id as u64,
        key: key as u64,
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
