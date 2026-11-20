use core::sync::atomic::{AtomicU32, Ordering};

use crate::{
    IVC_REGION_MAGIC, IVC_REGION_VERSION,
    endpoint::IvcEndpoints,
    ring::{IvcRing, IvcRingDirection},
};

const RING_SIZE: u32 = core::mem::size_of::<IvcRing>() as u32;
const PUBLISHER_TO_SUBSCRIBER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, publisher_to_subscriber) as u32;
const SUBSCRIBER_TO_PUBLISHER_RING_OFFSET: u32 =
    core::mem::offset_of!(IvcRegion, subscriber_to_publisher) as u32;
const IVC_REGION_FEATURE_SPSC_OPAQUE_SLOTS: u32 = 1;

/// Full opaque-slot IVC region for one publisher/subscriber pair.
///
/// Axvisor enforces at most one subscriber for the current SPSC protocol. The
/// first two fields intentionally match `axvm::runtime::ivc::IVCChannelHeader`.
/// Axvisor initializes them when the host-side channel is created. The remaining
/// fields are owned by this shared-memory protocol.
///
/// The rings begin at byte offsets 256 and 8704, and the complete region
/// occupies 17152 bytes with 256-byte alignment. Peers must agree on these
/// layout parameters as well as the region version.
#[repr(C, align(256))]
pub struct IvcRegion {
    publisher_id: u64,
    key: u64,
    header: IvcRegionHeader,
    publisher_to_subscriber: IvcRing,
    subscriber_to_publisher: IvcRing,
}

// SAFETY: The leading host-owned header fields are initialized before the
// backing is mapped and are never rewritten by guests. Protocol metadata uses
// atomics, so subscribers may poll readiness while a fresh region initializes.
// Ring slot bytes are accessed only by the sole producer and consumer under the
// Release/Acquire protocol. The unsafe initialization and attachment contracts
// require one initializer at a time, no endpoint or in-flight slot operation
// while ring and protocol metadata are reset, no attachment until both rings and
// that metadata are initialized and the final magic is published, and no reset
// while any endpoint remains live. The final magic Release linearizes
// initialization, and no shared metadata writes follow it. Under those
// conditions, concurrent shared access to the region is sound.
unsafe impl Sync for IvcRegion {}

impl IvcRegion {
    /// Initializes guest-owned protocol metadata and resets both rings.
    ///
    /// Each initialization has exactly one caller. On fresh Axvisor backing,
    /// the protocol magic starts at zero, the leading host header is already
    /// installed, and a subscriber may poll [`Self::protocol_header_matches`]
    /// concurrently. The subscriber must not attach until both rings and the
    /// protocol metadata are initialized and the final magic is published with
    /// Release and observed with Acquire. The publication is the completion
    /// point; attachment does not depend on the initializer's local function
    /// having returned. The host's channel-published state only means the
    /// mapping is available; it does not mean guest protocol initialization has
    /// completed.
    ///
    /// Reusing an already initialized generation requires the complete
    /// quiescence guarantees in the [Safety](#safety) section below. Slot
    /// payloads are not cleared: the sole producer overwrites a slot before
    /// publishing it with a Release `tail`, and its consumer reads that slot
    /// only after acquiring `tail`.
    ///
    /// ```compile_fail,E0133
    /// use axivc::IvcRegion;
    ///
    /// fn reset_shared_region(region: &mut IvcRegion) {
    ///     region.initialize();
    /// }
    /// ```
    ///
    /// # Safety
    ///
    /// The caller must guarantee all of the following:
    ///
    /// - The region mapping is valid, writable, and properly aligned for this
    ///   entire call and for the protocol operations that follow it.
    /// - The caller is the sole initializer for this initialization. No other
    ///   execution context may call `initialize` concurrently. A later
    ///   reinitialization is permitted only after the quiescence conditions
    ///   below are satisfied.
    /// - For first initialization, the backing is fresh and its protocol magic
    ///   is still zero before this call. Atomic readiness polling is permitted,
    ///   but no publisher or subscriber endpoint may exist or perform a slot
    ///   operation until both rings and protocol metadata are initialized and
    ///   the final magic Release is published.
    /// - For reinitialization, every publisher and subscriber endpoint and
    ///   every in-flight send or receive operation in every address space
    ///   sharing the region has already stopped. New attachment and any stale
    ///   readiness result must not race with initialization.
    /// - Every attachment must happen after this generation's initialization.
    ///   The same-thread call completion or ordinary thread synchronization can
    ///   establish that ordering locally. Another address space that learns of
    ///   completion through the shared header must use
    ///   [`Self::protocol_header_matches`] so its Acquire load observes the final
    ///   magic Release. Attachment only requires the Release publication and
    ///   preceding initialization to have completed; it does not require the
    ///   initializer's local method call to have returned elsewhere.
    /// - No endpoint remains live when a later reset begins. Dropping local
    ///   endpoints or clearing the magic does not revoke access in another
    ///   address space and is not sufficient proof of quiescence.
    pub unsafe fn initialize(&self) {
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

    /// Returns the host-provided publisher VM ID for this channel.
    pub fn publisher_id(&self) -> usize {
        self.publisher_id as usize
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
    /// The caller must guarantee that initialization happens before attachment
    /// for the current, stable generation. The same-thread call completion or
    /// ordinary thread synchronization may establish that ordering. If another
    /// address space learns completion through the shared header, it must use
    /// [`Self::protocol_header_matches`] so its Acquire load observes the final
    /// magic Release.
    ///
    /// The region mapping must remain valid. No initialization or reset may
    /// begin while any endpoint for either role or any in-flight slot operation
    /// remains live.
    ///
    /// The caller must also guarantee that the publisher role is attached only
    /// once across every address space sharing this region. Attaching it again
    /// would create duplicate producer and consumer endpoints, allowing data
    /// races on slot bytes.
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
    /// The caller must guarantee that initialization happens before attachment
    /// for the current, stable generation. The same-thread call completion or
    /// ordinary thread synchronization may establish that ordering. If another
    /// address space learns completion through the shared header, it must use
    /// [`Self::protocol_header_matches`] so its Acquire load observes the final
    /// magic Release.
    ///
    /// The region mapping must remain valid. No initialization or reset may
    /// begin while any endpoint for either role or any in-flight slot operation
    /// remains live.
    ///
    /// The caller must also guarantee that the subscriber role is attached only
    /// once across every address space sharing this region. Attaching it again
    /// would create duplicate producer and consumer endpoints, allowing data
    /// races on slot bytes.
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
            && self.features.load(Ordering::Relaxed) == IVC_REGION_FEATURE_SPSC_OPAQUE_SLOTS
            && self.publisher_to_subscriber_offset.load(Ordering::Relaxed)
                == PUBLISHER_TO_SUBSCRIBER_RING_OFFSET
            && self.subscriber_to_publisher_offset.load(Ordering::Relaxed)
                == SUBSCRIBER_TO_PUBLISHER_RING_OFFSET
            && self.ring_size.load(Ordering::Relaxed) == RING_SIZE
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
            .store(IVC_REGION_FEATURE_SPSC_OPAQUE_SLOTS, Ordering::Relaxed);
        self.publisher_to_subscriber_offset
            .store(PUBLISHER_TO_SUBSCRIBER_RING_OFFSET, Ordering::Relaxed);
        self.subscriber_to_publisher_offset
            .store(SUBSCRIBER_TO_PUBLISHER_RING_OFFSET, Ordering::Relaxed);
        self.ring_size.store(RING_SIZE, Ordering::Relaxed);
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

#[cfg(test)]
mod tests {
    use core::mem::{align_of, offset_of, size_of};

    use super::*;

    #[test]
    fn protocol_region_matches_linux_shared_memory_abi() {
        // The shared-memory ABI fixes these offsets and sizes. A round trip
        // alone cannot detect both endpoints using the same incompatible layout.
        assert_eq!(size_of::<IvcRegion>(), 17152);
        assert_eq!(align_of::<IvcRegion>(), 256);
        assert_eq!(size_of::<IvcRegionHeader>(), 32);
        assert_eq!(offset_of!(IvcRegion, publisher_id), 0);
        assert_eq!(offset_of!(IvcRegion, key), 8);
        assert_eq!(offset_of!(IvcRegion, header), 16);
        assert_eq!(offset_of!(IvcRegion, publisher_to_subscriber), 256);
        assert_eq!(offset_of!(IvcRegion, subscriber_to_publisher), 8704);

        let region = new_region_for_test(1, 0x4956_4301);
        assert!(!region.protocol_header_matches());
        // SAFETY: this test-private region is freshly constructed and has not
        // been shared; no endpoint exists yet.
        unsafe { region.initialize() };
        assert!(region.channel_header_matches(1, 0x4956_4301));
        assert!(region.protocol_header_matches());

        let header = &region.header;
        // Verify published metadata against the shared-memory layout, then
        // ensure a v2 peer or a v3 peer with the old layout is rejected.
        for (field, expected, incompatible) in [
            (&header.magic, 0x4956_4332, 0),
            (&header.version.0, 3, 2),
            (&header.header_size.0, 32, 0),
            (&header.region_size, 17152, 2240),
            (&header.features, 1, 0),
            (&header.publisher_to_subscriber_offset, 256, 64),
            (&header.subscriber_to_publisher_offset, 8704, 1152),
            (&header.ring_size, 8448, 1088),
        ] {
            assert_eq!(field.load(Ordering::Relaxed), expected);
            field.store(incompatible, Ordering::Relaxed);
            assert!(!region.protocol_header_matches());
            field.store(expected, Ordering::Relaxed);
        }
        assert!(region.protocol_header_matches());
    }
}
