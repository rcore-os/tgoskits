//! Link identity, peer lifecycle, and the process-level registry.
//!
//! The registry is the only cross-VM entry point. It is created once by the
//! AxVisor assembly and injected explicitly into every VM configuration
//! build; links are shared by `Arc` and disappear when the last reservation
//! or attachment drops. Peer slots never return to vacant inside one link:
//! a released reservation retires the slot until the whole link is
//! recreated, which also recreates the zeroed backing.
//!
//! Lock order is registry -> link state -> peer epochs -> backing. Shared
//! backing allocation and retirement scrubbing happen outside the registry
//! and link state locks; sink callbacks run after every lock is released.

use alloc::{boxed::Box, collections::BTreeMap, string::String, sync::Arc, vec::Vec};
use core::{
    fmt,
    sync::atomic::{AtomicU32, Ordering},
};

use ax_sync::RawSpinLock;

use super::{
    SharedBarBacking,
    backing::SharedBackingAllocator,
    doorbell::{Doorbell, DoorbellEvent, IvshmemEventSink},
    error::IvshmemError,
    layout::IvshmemMemoryLayout,
};

/// The only doorbell vector supported by the current link profile.
const SUPPORTED_DOORBELL_VECTOR: u16 = 0;

/// Interval between repeated doorbell diagnostics: the first ignored write
/// logs immediately and every 1024th repetition of the same reason logs
/// again.
const DOORBELL_DIAGNOSTIC_INTERVAL: u32 = 1024;

/// Per-reason counters for ignored doorbell writes.
///
/// Diagnostics never change guest-visible behaviour, so the counters carry no
/// synchronisation role and use `Relaxed` ordering; they only drive the
/// rate-limited logging decision.
#[derive(Default)]
struct IgnoredDoorbellCounters {
    unsupported_vector: AtomicU32,
    out_of_profile: AtomicU32,
    not_attached: AtomicU32,
    delivery_failed: AtomicU32,
}

impl IgnoredDoorbellCounters {
    /// Increments `counter` and returns whether this occurrence should log.
    fn should_log(counter: &AtomicU32) -> bool {
        let occurrence = counter.fetch_add(1, Ordering::Relaxed) + 1;
        occurrence == 1 || occurrence.is_multiple_of(DOORBELL_DIAGNOSTIC_INTERVAL)
    }
}

/// Stable identity of one shared-memory link inside the AxVisor process.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct LinkId(u32);

impl LinkId {
    /// Creates a link identity from its raw configuration value.
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    /// Returns the raw configuration value.
    pub const fn value(self) -> u32 {
        self.0
    }
}

impl fmt::Display for LinkId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Existing profile ceiling: 15 state entries fit in one state-table page and
/// fifteen 4 KiB outputs still fit the legacy 64 KiB BAR2 shape.
pub const MAX_PEERS_LIMIT: u16 = 15;

/// Largest BAR2 the current 64 MiB guest PCI memory aperture can host while
/// leaving room for BAR0 and the MSI-X BAR of this function.
const MAX_BAR2_SIZE: u64 = 0x0200_0000;

/// Guest cache policy of the BAR2 shared-memory transport.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Bar2CachePolicy {
    /// Legacy Normal Non-cacheable guest mappings over the shared backing.
    NonCacheable,
    /// Opt-in Normal Inner Shareable Write-Back guest mappings.
    ///
    /// The host administrator must provision a cache-coherent host and only
    /// attach managed guests that use compatible Normal Write-Back mappings.
    WriteBack,
}

/// Layout parameters of one link, identical across all its reservations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LinkProfile {
    max_peers: u16,
    common_size: u64,
    output_size: u64,
    bar2_size: u64,
    bar2_cache_policy: Bar2CachePolicy,
}

impl LinkProfile {
    /// The frozen double-peer baseline of F2–F7: two peers, no common
    /// section, one 28 KiB output page-range per peer, and the legacy
    /// non-cacheable BAR2 policy.
    pub const fn baseline() -> Self {
        Self {
            max_peers: 2,
            common_size: 0,
            output_size: 0x7000,
            bar2_size: 0x1_0000,
            bar2_cache_policy: Bar2CachePolicy::NonCacheable,
        }
    }

    /// Creates one validated profile.
    ///
    /// Rules: `1 <= max_peers <= MAX_PEERS_LIMIT`; `common_size` is zero or
    /// a 4 KiB multiple; `output_size` is a positive 4 KiB multiple. The
    /// power-of-two BAR2 size is derived here so all link lifecycles use one
    /// checked result, with the historical 64 KiB size as its floor. The
    /// 32 MiB ceiling leaves the other half of the 64 MiB guest PCI memory
    /// aperture available for BAR0, MSI-X and placement. New profiles use the
    /// legacy non-cacheable BAR2 policy until [`Self::with_bar2_cache_policy`]
    /// explicitly selects another policy.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::InvalidProfile`] when any rule is violated.
    pub fn new(max_peers: u16, common_size: u64, output_size: u64) -> Result<Self, IvshmemError> {
        const PAGE: u64 = 0x1000;
        let reject = |detail: String| IvshmemError::InvalidProfile { detail };
        if max_peers == 0 || max_peers > MAX_PEERS_LIMIT {
            return Err(reject(alloc::format!(
                "max peers {max_peers} must be within 1..={MAX_PEERS_LIMIT}"
            )));
        }
        if !common_size.is_multiple_of(PAGE) {
            return Err(reject(alloc::format!(
                "common size {common_size:#x} is not {PAGE:#x}-aligned"
            )));
        }
        if output_size == 0 || !output_size.is_multiple_of(PAGE) {
            return Err(reject(alloc::format!(
                "output size {output_size:#x} must be a positive {PAGE:#x} multiple"
            )));
        }
        let sections_size = u64::from(max_peers)
            .checked_mul(output_size)
            .and_then(|outputs_size| PAGE.checked_add(common_size)?.checked_add(outputs_size))
            .ok_or_else(|| reject("BAR2 section sizes overflow u64".into()))?;
        let bar2_size = sections_size
            .checked_next_power_of_two()
            .ok_or_else(|| reject("BAR2 size overflows u64".into()))?
            .max(super::SHARED_MEMORY_SIZE);
        if bar2_size > MAX_BAR2_SIZE {
            return Err(reject(alloc::format!(
                "BAR2 size {bar2_size:#x} for {max_peers} peers exceeds the {MAX_BAR2_SIZE:#x} \
                 limit imposed by the 64 MiB guest PCI aperture"
            )));
        }
        Ok(Self {
            max_peers,
            common_size,
            output_size,
            bar2_size,
            bar2_cache_policy: Bar2CachePolicy::NonCacheable,
        })
    }

    /// Selects the BAR2 guest cache policy for this profile.
    ///
    /// Write-back is currently limited to exactly two peers and no common
    /// section. The host administrator is responsible for provisioning a
    /// coherent host and managed guests with compatible Normal Inner
    /// Shareable Write-Back mappings; this device boundary has no reliable
    /// host-coherence query.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::InvalidProfile`] when the requested policy
    /// violates those write-back constraints.
    pub fn with_bar2_cache_policy(
        mut self,
        bar2_cache_policy: Bar2CachePolicy,
    ) -> Result<Self, IvshmemError> {
        if bar2_cache_policy == Bar2CachePolicy::WriteBack {
            if self.max_peers != 2 {
                return Err(IvshmemError::InvalidProfile {
                    detail: alloc::format!(
                        "write-back BAR2 requires exactly 2 peers, but the profile has {}",
                        self.max_peers
                    ),
                });
            }
            if self.common_size != 0 {
                return Err(IvshmemError::InvalidProfile {
                    detail: alloc::format!(
                        "write-back BAR2 requires no common section, but the profile has {:#x} \
                         bytes",
                        self.common_size
                    ),
                });
            }
        }
        self.bar2_cache_policy = bar2_cache_policy;
        Ok(self)
    }

    /// Returns the configured peer count.
    pub const fn max_peers(self) -> u16 {
        self.max_peers
    }

    /// Returns the configured common-section size in bytes.
    pub const fn common_size(self) -> u64 {
        self.common_size
    }

    /// Returns the configured per-peer output-section size in bytes.
    pub const fn output_size(self) -> u64 {
        self.output_size
    }

    /// Returns the power-of-two BAR2 size derived from this profile.
    pub const fn bar2_size(self) -> u64 {
        self.bar2_size
    }

    /// Returns the BAR2 guest cache policy.
    pub const fn bar2_cache_policy(self) -> Bar2CachePolicy {
        self.bar2_cache_policy
    }
}

/// Monotonic lifetime counter of one link object.
///
/// The value increments every time the link transitions from "no
/// reservations and no attachments" back to an active reservation. Any
/// reference captured before the transition is stale.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct LinkGeneration(u64);

impl LinkGeneration {
    /// Creates the initial generation of a fresh link.
    pub const fn initial() -> Self {
        Self(0)
    }

    /// Returns the raw generation value.
    pub const fn value(self) -> u64 {
        self.0
    }

    /// Returns the next generation.
    const fn next(self) -> Self {
        Self(self.0 + 1)
    }
}

impl core::fmt::Display for LinkGeneration {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Identity of one peer slot inside a link.
///
/// The range is checked against the link profile when a reservation is
/// created, not by this newtype.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PeerId(u16);

impl PeerId {
    /// Creates a peer identity from its raw configuration value.
    pub const fn new(value: u16) -> Self {
        Self(value)
    }

    /// Returns the raw configuration value.
    pub const fn value(self) -> u16 {
        self.0
    }
}

impl fmt::Display for PeerId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Lifecycle state of one peer slot.
#[derive(Clone, Copy, Debug)]
enum PeerSlot {
    /// Never reserved by any configuration.
    Vacant,
    /// Held by one configuration identity; no runtime attached yet.
    Reserved,
    /// Bound to one runtime endpoint build.
    Attached { generation: u64 },
    /// The reservation was released; the slot blocks until the link is
    /// destroyed.
    Retired,
}

/// Peer-slot table protected by one link-local lock.
struct LinkPeers {
    next_attachment_generation: u64,
    slots: Box<[PeerSlot]>,
    /// Live attachment count; a link with attachments never goes inactive.
    attached: usize,
    /// Per-attachment event sinks; entries follow the slot lifecycle:
    /// attach clears any stale entry, `set_event_sink` fills it, detach
    /// removes it.
    sinks: BTreeMap<PeerId, Arc<dyn IvshmemEventSink>>,
}

/// Fixed-size survivor snapshot built while the link lock is held.
struct SinkBatch {
    slots: [Option<Arc<dyn IvshmemEventSink>>; MAX_PEERS_LIMIT as usize],
    len: usize,
}

impl SinkBatch {
    fn new() -> Self {
        Self {
            slots: core::array::from_fn(|_| None),
            len: 0,
        }
    }

    fn push(&mut self, sink: Arc<dyn IvshmemEventSink>) {
        if let Some(slot) = self.slots.get_mut(self.len) {
            *slot = Some(sink);
            self.len += 1;
        }
    }

    fn iter(&self) -> impl Iterator<Item = &Arc<dyn IvshmemEventSink>> {
        self.slots[..self.len].iter().filter_map(Option::as_ref)
    }
}

/// Lifecycle state of one link object.
enum LinkState {
    /// No reservations and no attachments. The retired backing is detached
    /// for zeroing outside this lock; the skeleton (id, profile, generation)
    /// stays in the registry for reattach validation.
    Inactive { generation: LinkGeneration },
    /// At least one reservation or attachment; backing allocated.
    Active {
        generation: LinkGeneration,
        backing: Arc<SharedBarBacking>,
        peers: LinkPeers,
    },
}

/// Shared state of one ivshmem link: profile, backing, and peer slots.
pub struct IvshmemLink {
    id: LinkId,
    profile: LinkProfile,
    layout: IvshmemMemoryLayout,
    state: RawSpinLock<LinkState>,
    /// Host-owned lifecycle epochs survive inactive periods and backing
    /// replacement. The initial nonzero value distinguishes a live observed
    /// generation from never-initialized guest state.
    peer_epochs: RawSpinLock<[u64; MAX_PEERS_LIMIT as usize]>,
    allocator: Arc<dyn SharedBackingAllocator>,
    ignored_doorbell: IgnoredDoorbellCounters,
}

impl IvshmemLink {
    fn new(
        id: LinkId,
        profile: LinkProfile,
        allocator: Arc<dyn SharedBackingAllocator>,
    ) -> Result<Self, IvshmemError> {
        let layout = IvshmemMemoryLayout::derive(profile.bar2_size(), profile)?;
        Ok(Self {
            id,
            profile,
            layout,
            state: RawSpinLock::new(LinkState::Inactive {
                generation: LinkGeneration::initial(),
            }),
            peer_epochs: RawSpinLock::new([1; MAX_PEERS_LIMIT as usize]),
            allocator,
            ignored_doorbell: IgnoredDoorbellCounters::default(),
        })
    }

    /// Returns the link identity.
    pub const fn id(&self) -> LinkId {
        self.id
    }

    /// Returns the peer count of the frozen link profile.
    pub const fn max_peers(&self) -> u16 {
        self.profile.max_peers()
    }

    /// Returns the frozen link profile.
    pub const fn profile(&self) -> LinkProfile {
        self.profile
    }

    /// Returns the generation of the current (or most recently completed)
    /// lifecycle.
    pub fn generation(&self) -> LinkGeneration {
        match &*self.state.lock_irqsave() {
            LinkState::Inactive { generation } => *generation,
            LinkState::Active { generation, .. } => *generation,
        }
    }

    /// Returns the shared BAR2 backing of the active lifecycle.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::PeerNotReserved`] while the link is inactive
    /// (backing released, guest endpoints impossible).
    pub fn backing(&self) -> Result<Arc<SharedBarBacking>, IvshmemError> {
        let state = self.state.lock_irqsave();
        match &*state {
            LinkState::Active { backing, .. } => Ok(Arc::clone(backing)),
            LinkState::Inactive { .. } => Err(IvshmemError::PeerNotReserved {
                link: self.id.value(),
                peer: 0,
            }),
        }
    }

    /// Returns the BAR2 size of the link profile.
    pub const fn bar2_size(&self) -> u64 {
        self.profile.bar2_size()
    }

    /// Returns the frozen BAR2 layout of this link.
    pub const fn layout(&self) -> &IvshmemMemoryLayout {
        &self.layout
    }

    /// Publishes the peer's BAR0 State value into its state-table entry.
    ///
    /// Called by the owning endpoint's BAR0 State write path, outside the
    /// endpoint register lock.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::PeerOutOfProfile`] for an unknown peer and
    /// [`IvshmemError::SharedMemoryOutOfRange`] if the entry write would
    /// leave the backing (a layout bug, not a guest condition).
    pub fn publish_state(&self, peer: PeerId, value: u32) -> Result<(), IvshmemError> {
        let offset = self
            .layout
            .state_entry_offset(peer, self.profile.max_peers())?;
        let backing = self.backing()?;
        backing.write_u32_release(offset, value)
    }

    /// Clears the peer's state-table entry; called from endpoint reset.
    /// The same errors as `publish_state()` apply.
    pub fn clear_state(&self, peer: PeerId) -> Result<(), IvshmemError> {
        self.publish_state(peer, 0)
    }

    /// Clears one attached peer's STATE entry, advances its host-owned
    /// lifecycle epoch, and notifies every other attached peer.
    ///
    /// The backing remains the same lifecycle allocation, so payload and
    /// protocol bytes not owned by guest STATE stay available for recovery.
    /// The epoch is written before any sink runs. Sinks are invoked after the
    /// link state and backing locks have been released.
    fn reset_peer(&self, peer: PeerId, attachment_generation: u64) -> Result<(), IvshmemError> {
        let (sinks, epoch) = {
            let state = self.state.lock_irqsave();
            let LinkState::Active { backing, peers, .. } = &*state else {
                return Err(IvshmemError::PeerNotReserved {
                    link: self.id.value(),
                    peer: peer.value(),
                });
            };
            let attached = matches!(
                peers.slots.get(usize::from(peer.value())),
                Some(PeerSlot::Attached { generation })
                    if *generation == attachment_generation
            );
            if !attached {
                return Err(IvshmemError::PeerNotReserved {
                    link: self.id.value(),
                    peer: peer.value(),
                });
            }
            let state_offset = self
                .layout
                .state_entry_offset(peer, self.profile.max_peers())?;
            backing.write_u32_release(state_offset, 0)?;
            let epoch = self.advance_epoch(peer)?;
            let epoch_offset = self
                .layout
                .peer_epoch_offset(peer, self.profile.max_peers())?;
            backing.write_u64_release(epoch_offset, epoch)?;
            (Self::surviving_sinks(peers, peer), epoch)
        };
        self.notify_epoch_change(sinks, peer, epoch);
        Ok(())
    }

    /// Routes one doorbell from the writing endpoint to the target peer.
    ///
    /// Out-of-profile targets, inactive targets, and unsupported vectors are
    /// specification-defined no-ops with rate-limited diagnostics; a guest
    /// doorbell never becomes a device access error or a VM abort. The
    /// target sink is cloned out of the peer table and invoked after the
    /// lock is released, so sink implementations never run under the
    /// peer-table lock.
    pub fn deliver_doorbell(&self, source: PeerId, doorbell: Doorbell) {
        let target = doorbell.target();
        if !matches!(&*self.state.lock_irqsave(), LinkState::Active { .. }) {
            // The only guest-triggered path stays a specification no-op
            // while the link is inactive; sinks cannot exist anyway.
            return;
        }
        if doorbell.vector() != SUPPORTED_DOORBELL_VECTOR {
            if IgnoredDoorbellCounters::should_log(&self.ignored_doorbell.unsupported_vector) {
                warn!(
                    "ivshmem link {} ignored a doorbell from peer {source}: vector {} is not \
                     supported",
                    self.id,
                    doorbell.vector()
                );
            }
            return;
        }
        if usize::from(target.value()) >= usize::from(self.max_peers()) {
            if IgnoredDoorbellCounters::should_log(&self.ignored_doorbell.out_of_profile) {
                warn!(
                    "ivshmem link {} ignored a doorbell from peer {source}: target peer {target} \
                     is outside the profile of {} peers",
                    self.id,
                    self.max_peers()
                );
            }
            return;
        }
        let Some(sink) = self.attached_sink(target) else {
            return;
        };
        let event = DoorbellEvent::new(source, doorbell);
        if let Err(error) = sink.deliver(event) {
            // A sink failure is a recoverable internal problem: the write is
            // ignored and no state is rolled back.
            if IgnoredDoorbellCounters::should_log(&self.ignored_doorbell.delivery_failed) {
                warn!(
                    "ivshmem link {} ignored a doorbell from peer {source} to peer {target}: \
                     {error}",
                    self.id
                );
            }
        }
    }

    /// Returns the attached target's sink, or `None` when the doorbell must
    /// be ignored.
    fn attached_sink(&self, target: PeerId) -> Option<Arc<dyn IvshmemEventSink>> {
        let state = self.state.lock_irqsave();
        let LinkState::Active { peers, .. } = &*state else {
            return None;
        };
        let attached = matches!(
            peers.slots.get(usize::from(target.value())),
            Some(PeerSlot::Attached { .. })
        );
        let sink = if attached {
            peers.sinks.get(&target).cloned()
        } else {
            None
        };
        drop(state);
        if !attached && IgnoredDoorbellCounters::should_log(&self.ignored_doorbell.not_attached) {
            warn!(
                "ivshmem link {} ignored a doorbell to peer {target}: no endpoint is attached",
                self.id
            );
        }
        sink
    }

    /// Reserves one peer, activating an inactive lifecycle in the same state
    /// transition when necessary.
    ///
    /// The backing allocation and peer-table setup happen before the final
    /// state lock. If another caller activated the link meanwhile, this
    /// caller reserves on that lifecycle and the unused backing is dropped
    /// after releasing the lock.
    fn reserve_peer(&self, peer: PeerId) -> Result<(), IvshmemError> {
        if peer.value() >= self.profile.max_peers() {
            return Err(IvshmemError::PeerOutOfProfile {
                peer: peer.value(),
                max_peers: self.profile.max_peers(),
            });
        }

        {
            let mut state = self.state.lock_irqsave();
            if let LinkState::Active { peers, .. } = &mut *state {
                return self.reserve_slot(peers, peer);
            }
        }

        let peers = self.vacant_peers(peer)?;
        let backing = Arc::new(SharedBarBacking::try_new(
            self.profile.bar2_size(),
            Arc::clone(&self.allocator),
        )?);

        let mut state = self.state.lock_irqsave();
        let active_result = match &mut *state {
            LinkState::Active { peers, .. } => Some(self.reserve_slot(peers, peer)),
            LinkState::Inactive { .. } => None,
        };
        if let Some(result) = active_result {
            drop(state);
            return result;
        }
        let generation = match &*state {
            LinkState::Inactive { generation } => *generation,
            LinkState::Active { .. } => unreachable!("active state was handled above"),
        };
        let epochs = self.peer_epoch_snapshot();
        for peer in 0..self.profile.max_peers() {
            let offset = self
                .layout
                .peer_epoch_offset(PeerId::new(peer), self.profile.max_peers())?;
            backing.write_u64_release(offset, epochs[usize::from(peer)])?;
        }
        *state = LinkState::Active {
            generation,
            backing,
            peers,
        };
        Ok(())
    }

    fn vacant_peers(&self, reserved: PeerId) -> Result<LinkPeers, IvshmemError> {
        let max_peers = self.profile.max_peers();
        let mut slots = Vec::new();
        slots
            .try_reserve_exact(usize::from(max_peers))
            .map_err(|_| IvshmemError::AllocationFailed {
                operation: "allocate ivshmem peer table",
            })?;
        slots.resize(usize::from(max_peers), PeerSlot::Vacant);
        slots[usize::from(reserved.value())] = PeerSlot::Reserved;
        Ok(LinkPeers {
            next_attachment_generation: 0,
            slots: slots.into_boxed_slice(),
            attached: 0,
            sinks: BTreeMap::new(),
        })
    }

    fn reserve_slot(&self, peers: &mut LinkPeers, peer: PeerId) -> Result<(), IvshmemError> {
        let Some(slot) = peers.slots.get_mut(usize::from(peer.value())) else {
            return Err(IvshmemError::PeerOutOfProfile {
                peer: peer.value(),
                max_peers: self.profile.max_peers(),
            });
        };
        match slot {
            PeerSlot::Vacant => {
                *slot = PeerSlot::Reserved;
                Ok(())
            }
            PeerSlot::Reserved | PeerSlot::Attached { .. } => {
                Err(IvshmemError::PeerAlreadyReserved {
                    link: self.id.value(),
                    peer: peer.value(),
                })
            }
            PeerSlot::Retired => Err(IvshmemError::PeerRetired {
                link: self.id.value(),
                peer: peer.value(),
            }),
        }
    }

    fn attach_slot(&self, peer: PeerId) -> Result<u64, IvshmemError> {
        let mut state = self.state.lock_irqsave();
        let LinkState::Active { peers, .. } = &mut *state else {
            return Err(IvshmemError::PeerNotReserved {
                link: self.id.value(),
                peer: peer.value(),
            });
        };
        let index = usize::from(peer.value());
        match peers.slots.get(index) {
            Some(PeerSlot::Reserved) => {}
            Some(PeerSlot::Attached { .. }) => {
                return Err(IvshmemError::PeerAlreadyAttached {
                    link: self.id.value(),
                    peer: peer.value(),
                });
            }
            Some(PeerSlot::Retired) => {
                return Err(IvshmemError::PeerRetired {
                    link: self.id.value(),
                    peer: peer.value(),
                });
            }
            Some(PeerSlot::Vacant) | None => {
                return Err(IvshmemError::PeerNotReserved {
                    link: self.id.value(),
                    peer: peer.value(),
                });
            }
        }
        peers.next_attachment_generation = peers.next_attachment_generation.wrapping_add(1);
        peers.attached += 1;
        let generation = peers.next_attachment_generation;
        peers.slots[index] = PeerSlot::Attached { generation };
        // A re-attached slot must not keep a sink registered by a previous
        // attachment; the new endpoint registers its own sink.
        peers.sinks.remove(&peer);
        Ok(generation)
    }

    fn detach_slot(&self, peer: PeerId, attachment_generation: u64) {
        let (retired, epoch_change) = {
            let mut state = self.state.lock_irqsave();
            let current_generation = match &*state {
                LinkState::Active { generation, .. } => *generation,
                LinkState::Inactive { .. } => return,
            };
            let LinkState::Active { peers, .. } = &mut *state else {
                return;
            };
            let index = usize::from(peer.value());
            let is_current = match peers.slots.get(index) {
                Some(PeerSlot::Attached {
                    generation: current,
                }) if *current == attachment_generation => true,
                Some(PeerSlot::Attached { .. }) => return,
                Some(PeerSlot::Retired) => false,
                Some(PeerSlot::Reserved) | Some(PeerSlot::Vacant) | None => return,
            };
            // Every attach pairs with exactly one detach: the count tracks the
            // attachment object, not the slot state (a released reservation
            // retires the slot before its endpoint detaches).
            peers.attached = peers.attached.saturating_sub(1);
            // This attachment owns one sink entry even when its reservation
            // already retired the slot before the endpoint drop. A stale
            // handle must not remove a newer attachment's sink.
            peers.sinks.remove(&peer);
            if is_current {
                // A stale attachment must not clear a newer attachment's state.
                peers.slots[index] = PeerSlot::Reserved;
            }
            if let LinkState::Active { backing, .. } = &*state {
                match self
                    .layout
                    .state_entry_offset(peer, self.profile.max_peers())
                    .and_then(|offset| backing.write_u32_release(offset, 0))
                {
                    Ok(()) => {}
                    Err(error) => warn!(
                        "ivshmem link {} could not clear peer {peer} state on detach: {error}",
                        self.id
                    ),
                }
            }
            let Ok(epoch) = self.advance_epoch(peer) else {
                warn!(
                    "ivshmem link {} could not advance peer {peer} detach epoch",
                    self.id
                );
                return;
            };
            let retired = self.maybe_deactivate(&mut state, &current_generation);
            let epoch_change = if retired.is_some() {
                None
            } else {
                match self.publish_epoch_change(&state, peer, epoch) {
                    Ok(sinks) => Some((sinks, epoch)),
                    Err(error) => {
                        warn!(
                            "ivshmem link {} could not publish peer {peer} detach epoch {epoch}: \
                             {error}",
                            self.id
                        );
                        None
                    }
                }
            };
            (retired, epoch_change)
        };
        if let Some(backing) = retired {
            backing.zero();
        }
        if let Some((sinks, epoch)) = epoch_change {
            self.notify_epoch_change(sinks, peer, epoch);
        }
    }

    fn release_slot(&self, peer: PeerId) {
        let (retired, epoch_change) = {
            let mut state = self.state.lock_irqsave();
            let generation = match &mut *state {
                LinkState::Active { generation, .. } => *generation,
                LinkState::Inactive { .. } => return,
            };
            let LinkState::Active { peers, .. } = &mut *state else {
                return;
            };
            match peers.slots.get_mut(usize::from(peer.value())) {
                Some(PeerSlot::Retired) => return,
                Some(slot) => *slot = PeerSlot::Retired,
                None => return,
            }
            if let LinkState::Active { backing, .. } = &*state {
                match self
                    .layout
                    .state_entry_offset(peer, self.profile.max_peers())
                    .and_then(|offset| backing.write_u32_release(offset, 0))
                {
                    Ok(()) => {}
                    Err(error) => warn!(
                        "ivshmem link {} could not clear peer {peer} state on release: {error}",
                        self.id
                    ),
                }
            }
            let Ok(epoch) = self.advance_epoch(peer) else {
                warn!(
                    "ivshmem link {} could not advance peer {peer} release epoch",
                    self.id
                );
                return;
            };
            let retired = self.maybe_deactivate(&mut state, &generation);
            let epoch_change = if retired.is_some() {
                None
            } else {
                match self.publish_epoch_change(&state, peer, epoch) {
                    Ok(sinks) => Some((sinks, epoch)),
                    Err(error) => {
                        warn!(
                            "ivshmem link {} could not publish peer {peer} release epoch {epoch}: \
                             {error}",
                            self.id
                        );
                        None
                    }
                }
            };
            (retired, epoch_change)
        };
        if let Some(backing) = retired {
            backing.zero();
        }
        if let Some((sinks, epoch)) = epoch_change {
            self.notify_epoch_change(sinks, peer, epoch);
        }
    }

    fn advance_epoch(&self, peer: PeerId) -> Result<u64, IvshmemError> {
        if peer.value() >= self.profile.max_peers() {
            return Err(IvshmemError::PeerOutOfProfile {
                peer: peer.value(),
                max_peers: self.profile.max_peers(),
            });
        }
        let mut epochs = self.peer_epochs.lock_irqsave();
        let epoch = epochs[usize::from(peer.value())].wrapping_add(1);
        epochs[usize::from(peer.value())] = if epoch == 0 { 1 } else { epoch };
        Ok(epochs[usize::from(peer.value())])
    }

    fn peer_epoch_snapshot(&self) -> [u64; MAX_PEERS_LIMIT as usize] {
        *self.peer_epochs.lock_irqsave()
    }

    fn publish_epoch_change(
        &self,
        state: &LinkState,
        peer: PeerId,
        epoch: u64,
    ) -> Result<SinkBatch, IvshmemError> {
        let LinkState::Active { backing, peers, .. } = state else {
            return Ok(SinkBatch::new());
        };
        let offset = self
            .layout
            .peer_epoch_offset(peer, self.profile.max_peers())?;
        backing.write_u64_release(offset, epoch)?;
        Ok(Self::surviving_sinks(peers, peer))
    }

    fn surviving_sinks(peers: &LinkPeers, source: PeerId) -> SinkBatch {
        let mut sinks = SinkBatch::new();
        for (peer, sink) in &peers.sinks {
            if *peer != source
                && matches!(
                    peers.slots.get(usize::from(peer.value())),
                    Some(PeerSlot::Attached { .. })
                )
            {
                sinks.push(Arc::clone(sink));
            }
        }
        sinks
    }

    fn notify_epoch_change(&self, sinks: SinkBatch, peer: PeerId, epoch: u64) {
        for sink in sinks.iter() {
            if let Err(error) = sink.peer_epoch_changed(peer, epoch)
                && IgnoredDoorbellCounters::should_log(&self.ignored_doorbell.delivery_failed)
            {
                warn!(
                    "ivshmem link {} could not notify a survivor about peer {peer} epoch {epoch}: \
                     {error}",
                    self.id
                );
            }
        }
    }

    /// Ends the lifecycle when the last reservation and attachment are gone:
    /// the backing is detached for zeroing outside the lifecycle lock, the
    /// peer table resets for the next lifecycle, and the generation advances.
    fn maybe_deactivate(
        &self,
        state: &mut LinkState,
        generation: &LinkGeneration,
    ) -> Option<Arc<SharedBarBacking>> {
        let lifecycle_ended = match &mut *state {
            LinkState::Active { peers, .. } => {
                peers.attached == 0
                    && peers
                        .slots
                        .iter()
                        .all(|slot| matches!(slot, PeerSlot::Vacant | PeerSlot::Retired))
            }
            LinkState::Inactive { .. } => false,
        };
        if lifecycle_ended {
            let previous = core::mem::replace(
                state,
                LinkState::Inactive {
                    generation: generation.next(),
                },
            );
            let LinkState::Active { backing, .. } = previous else {
                unreachable!("lifecycle state changed while its lock was held")
            };
            Some(backing)
        } else {
            None
        }
    }
}

impl fmt::Debug for IvshmemLink {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IvshmemLink")
            .field("id", &self.id)
            .field("max_peers", &self.profile.max_peers())
            .finish_non_exhaustive()
    }
}

/// Exclusive configuration identity of one `(link, peer)` pair.
///
/// The endpoint model owns the reservation for the VM lifetime; dropping it
/// retires the peer slot until the whole link is destroyed.
pub struct PeerReservation {
    link: Arc<IvshmemLink>,
    peer: PeerId,
}

impl PeerReservation {
    /// Returns the link this reservation belongs to.
    pub fn link(&self) -> &Arc<IvshmemLink> {
        &self.link
    }

    /// Returns the reserved peer identity.
    pub const fn peer_id(&self) -> PeerId {
        self.peer
    }

    /// Attaches one runtime endpoint to the reserved peer.
    ///
    /// Bundle registration may fail and retry, so attaching stays available
    /// after a previous attachment dropped; only one attachment may exist at
    /// a time.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::PeerAlreadyAttached`] while another endpoint is
    /// still attached.
    pub fn attach(&self) -> Result<PeerAttachment, IvshmemError> {
        let generation = self.link.attach_slot(self.peer)?;
        Ok(PeerAttachment {
            link: Arc::clone(&self.link),
            peer: self.peer,
            generation,
        })
    }
}

impl Drop for PeerReservation {
    fn drop(&mut self) {
        self.link.release_slot(self.peer);
    }
}

impl fmt::Debug for PeerReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerReservation")
            .field("link", &self.link.id())
            .field("peer", &self.peer)
            .finish()
    }
}

/// Runtime binding of one endpoint to its reserved peer slot.
pub struct PeerAttachment {
    link: Arc<IvshmemLink>,
    peer: PeerId,
    generation: u64,
}

impl PeerAttachment {
    /// Returns the attached link.
    pub fn link(&self) -> &Arc<IvshmemLink> {
        &self.link
    }

    /// Returns the attached peer identity.
    pub const fn peer_id(&self) -> PeerId {
        self.peer
    }

    /// Returns whether this attachment still owns the current peer slot.
    ///
    /// A released reservation or a replacement attachment makes the handle
    /// stale. Stale handles must not publish resets or epochs.
    pub fn is_current(&self) -> bool {
        let state = self.link.state.lock_irqsave();
        let LinkState::Active { peers, .. } = &*state else {
            return false;
        };
        matches!(
            peers.slots.get(usize::from(self.peer.value())),
            Some(PeerSlot::Attached { generation }) if *generation == self.generation
        )
    }

    /// Clears and resets this exact attachment generation.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::PeerNotReserved`] when this attachment is stale
    /// or the link is no longer active.
    pub fn reset(&self) -> Result<(), IvshmemError> {
        self.link.reset_peer(self.peer, self.generation)
    }

    /// Registers this peer's event sink on the attached link.
    ///
    /// The sink receives doorbells addressed to this peer. Re-attaching a
    /// slot drops any sink left by a previous attachment, so endpoints must
    /// register the sink on every build.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::PeerNotReserved`] when this attachment is no
    /// longer the current generation of its peer slot.
    pub fn set_event_sink(&self, sink: Arc<dyn IvshmemEventSink>) -> Result<(), IvshmemError> {
        let mut state = self.link.state.lock_irqsave();
        let LinkState::Active { peers, .. } = &mut *state else {
            return Err(IvshmemError::PeerNotReserved {
                link: self.link.id.value(),
                peer: self.peer.value(),
            });
        };
        match peers.slots.get(usize::from(self.peer.value())) {
            Some(PeerSlot::Attached { generation }) if *generation == self.generation => {}
            _ => {
                return Err(IvshmemError::PeerNotReserved {
                    link: self.link.id.value(),
                    peer: self.peer.value(),
                });
            }
        }
        peers.sinks.insert(self.peer, sink);
        Ok(())
    }
}

impl Drop for PeerAttachment {
    fn drop(&mut self) {
        self.link.detach_slot(self.peer, self.generation);
    }
}

impl fmt::Debug for PeerAttachment {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PeerAttachment")
            .field("link", &self.link.id())
            .field("peer", &self.peer)
            .finish()
    }
}

/// Process-level owner of every ivshmem link.
///
/// The AxVisor assembly creates exactly one registry and injects it into all
/// VM configuration builds. Locking stays on the configuration path with the
/// `registry -> link` order; runtime BAR access never touches the registry.
pub struct IvshmemLinkRegistry {
    /// Link skeletons stay alive for the registry lifetime so reattach
    /// validation can compare generations after every peer left.
    links: RawSpinLock<BTreeMap<LinkId, Arc<IvshmemLink>>>,
    allocator: Arc<dyn SharedBackingAllocator>,
}

impl IvshmemLinkRegistry {
    /// Creates an empty registry over the given backing allocator.
    ///
    /// Every link created through this registry reserves its shared memory
    /// through `allocator`; the allocator must outlive all links, which the
    /// registry guarantees by holding one clone itself.
    pub fn new(allocator: Arc<dyn SharedBackingAllocator>) -> Self {
        Self {
            links: RawSpinLock::new(BTreeMap::new()),
            allocator,
        }
    }

    /// Reserves one peer of the named link, creating the link on first use.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::PeerOutOfProfile`] for peers outside the link
    /// profile, [`IvshmemError::PeerAlreadyReserved`] when another
    /// configuration holds the slot, [`IvshmemError::PeerRetired`] when the
    /// slot was released earlier, and [`IvshmemError::AllocationFailed`] when
    /// the link backing cannot be allocated.
    pub fn reserve(&self, link_id: u32, peer_id: u16) -> Result<PeerReservation, IvshmemError> {
        self.reserve_with_profile(link_id, peer_id, LinkProfile::baseline())
    }

    /// Reserves one peer, checking `profile` against the link's profile.
    ///
    /// The first reservation of a link fixes its profile; later reservations
    /// must match exactly.
    ///
    /// # Errors
    ///
    /// Returns [`IvshmemError::LinkProfileMismatch`] when the profile differs,
    /// in addition to the existing `reserve()` error set.
    pub fn reserve_with_profile(
        &self,
        link_id: u32,
        peer_id: u16,
        profile: LinkProfile,
    ) -> Result<PeerReservation, IvshmemError> {
        let id = LinkId::new(link_id);
        let peer = PeerId::new(peer_id);
        if peer.value() >= profile.max_peers() {
            return Err(IvshmemError::PeerOutOfProfile {
                peer: peer.value(),
                max_peers: profile.max_peers(),
            });
        }
        let check_profile = |link: &Arc<IvshmemLink>| {
            if link.profile() == profile {
                return Ok(());
            }
            Err(IvshmemError::LinkProfileMismatch {
                link: link_id,
                detail: alloc::format!(
                    "existing profile is {link_profile:?}, requested {requested:?}",
                    link_profile = link.profile(),
                    requested = profile
                ),
            })
        };
        let existing = {
            let links = self.links.lock_irqsave();
            links.get(&id).cloned()
        };
        if let Some(link) = existing {
            check_profile(&link)?;
            link.reserve_peer(peer)?;
            return Ok(PeerReservation { link, peer });
        }

        // A new link becomes visible only after its first backing and peer
        // reservation are valid. An allocation failure therefore leaves no
        // frozen profile in the registry and a later call may retry.
        let candidate = Arc::new(IvshmemLink::new(id, profile, Arc::clone(&self.allocator))?);
        candidate.reserve_peer(peer)?;
        let (link, already_reserved) = {
            let mut links = self.links.lock_irqsave();
            match links.get(&id).cloned() {
                Some(link) => (link, false),
                None => {
                    links.insert(id, Arc::clone(&candidate));
                    (candidate, true)
                }
            }
        };
        if !already_reserved {
            check_profile(&link)?;
            link.reserve_peer(peer)?;
        }
        Ok(PeerReservation { link, peer })
    }
}

impl fmt::Debug for IvshmemLinkRegistry {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("IvshmemLinkRegistry")
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use alloc::{sync::Arc, vec, vec::Vec};
    use core::sync::atomic::AtomicBool;
    use std::sync::Mutex;

    use super::*;
    use crate::ivshmem::{
        IvshmemRegisters, SHARED_MEMORY_SIZE,
        backing::{TestAllocator, test_allocator},
    };

    const MAX_PEERS: u16 = 2;

    fn registry() -> IvshmemLinkRegistry {
        IvshmemLinkRegistry::new(test_allocator())
    }

    /// Records every delivered event for routing assertions.
    struct EventRecorder {
        doorbells: Mutex<Vec<DoorbellEvent>>,
        epochs: Mutex<Vec<(PeerId, u64)>>,
    }

    impl EventRecorder {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                doorbells: Mutex::new(Vec::new()),
                epochs: Mutex::new(Vec::new()),
            })
        }

        fn events(&self) -> Vec<DoorbellEvent> {
            self.doorbells.lock().unwrap().clone()
        }

        fn epochs(&self) -> Vec<(PeerId, u64)> {
            self.epochs.lock().unwrap().clone()
        }
    }

    impl IvshmemEventSink for EventRecorder {
        fn deliver(&self, event: DoorbellEvent) -> Result<(), IvshmemError> {
            self.doorbells.lock().unwrap().push(event);
            Ok(())
        }

        fn peer_epoch_changed(&self, peer: PeerId, epoch: u64) -> Result<(), IvshmemError> {
            self.epochs.lock().unwrap().push((peer, epoch));
            Ok(())
        }
    }

    /// Mirrors the adapter sink: doorbells merge into one event-status bit.
    struct RegisterSink(Mutex<IvshmemRegisters>);

    impl RegisterSink {
        fn pending(&self) -> u32 {
            self.0
                .lock()
                .unwrap()
                .read(
                    crate::ivshmem::EVENT_STATUS_OFFSET,
                    PeerId::new(0),
                    crate::ivshmem::LinkProfile::baseline(),
                )
                .unwrap()
        }
    }

    impl IvshmemEventSink for RegisterSink {
        fn deliver(&self, _event: DoorbellEvent) -> Result<(), IvshmemError> {
            self.0.lock().unwrap().record_event();
            Ok(())
        }

        fn peer_epoch_changed(&self, _peer: PeerId, _epoch: u64) -> Result<(), IvshmemError> {
            self.0.lock().unwrap().record_event();
            Ok(())
        }
    }

    /// A sink that always fails, for the ignored-delivery path.
    struct FailingSink;

    impl IvshmemEventSink for FailingSink {
        fn deliver(&self, _event: DoorbellEvent) -> Result<(), IvshmemError> {
            Err(IvshmemError::EventDeliveryFailed {
                operation: "record doorbell event",
                detail: "test sink refused the event".into(),
            })
        }
    }

    /// Probes whether the link lifecycle lock is held while the sink runs.
    struct LockProbeSink {
        link: Arc<IvshmemLink>,
        lock_observed: AtomicBool,
    }

    impl IvshmemEventSink for LockProbeSink {
        fn deliver(&self, _event: DoorbellEvent) -> Result<(), IvshmemError> {
            self.lock_observed
                .store(self.link.state.is_locked(), Ordering::Relaxed);
            Ok(())
        }

        fn peer_epoch_changed(&self, _peer: PeerId, _epoch: u64) -> Result<(), IvshmemError> {
            self.lock_observed
                .store(self.link.state.is_locked(), Ordering::Relaxed);
            Ok(())
        }
    }

    /// Checks that the epoch is already visible and both link locks are free
    /// when a lifecycle notification reaches a survivor.
    struct EpochPublishProbe {
        link: Arc<IvshmemLink>,
        offset: u64,
        published: AtomicBool,
        lock_observed: AtomicBool,
    }

    impl IvshmemEventSink for EpochPublishProbe {
        fn deliver(&self, _event: DoorbellEvent) -> Result<(), IvshmemError> {
            Ok(())
        }

        fn peer_epoch_changed(&self, _peer: PeerId, epoch: u64) -> Result<(), IvshmemError> {
            self.lock_observed.store(
                self.link.state.is_locked() || self.link.peer_epochs.is_locked(),
                Ordering::Relaxed,
            );
            self.published.store(
                self.link
                    .backing()
                    .ok()
                    .and_then(|backing| backing.read_u64_acquire(self.offset).ok())
                    == Some(epoch),
                Ordering::Relaxed,
            );
            Ok(())
        }
    }

    fn doorbell(target: u16, vector: u16) -> Doorbell {
        Doorbell::from_write(((target as u32) << 16) | vector as u32)
    }

    fn attached_pair() -> (PeerReservation, PeerReservation) {
        let registry = registry();
        let peer0 = registry.reserve(1, 0).unwrap();
        let peer1 = registry.reserve(1, 1).unwrap();
        (peer0, peer1)
    }

    #[test]
    fn reserves_both_peers_of_one_link() {
        let registry = registry();
        let peer0 = registry.reserve(1, 0).unwrap();
        let peer1 = registry.reserve(1, 1).unwrap();
        assert!(Arc::ptr_eq(peer0.link(), peer1.link()));
        assert_eq!(peer0.link().max_peers(), MAX_PEERS);
        assert_eq!(peer0.link().bar2_size(), SHARED_MEMORY_SIZE);
    }

    #[test]
    fn rejects_duplicate_reservations_and_out_of_profile_peers() {
        let registry = registry();
        let _peer0 = registry.reserve(1, 0).unwrap();
        assert_eq!(
            registry.reserve(1, 0).unwrap_err(),
            IvshmemError::PeerAlreadyReserved { link: 1, peer: 0 }
        );
        assert_eq!(
            registry.reserve(1, 2).unwrap_err(),
            IvshmemError::PeerOutOfProfile {
                peer: 2,
                max_peers: MAX_PEERS
            }
        );
    }

    #[test]
    fn maximum_advertised_peer_count_fits_in_bar2() {
        let profile = LinkProfile::new(MAX_PEERS_LIMIT, 0, 0x1000).unwrap();
        let registry = registry();
        let last = registry
            .reserve_with_profile(3, MAX_PEERS_LIMIT - 1, profile)
            .unwrap();
        assert_eq!(last.link().max_peers(), MAX_PEERS_LIMIT);
        assert_eq!(last.link().bar2_size(), SHARED_MEMORY_SIZE);
        assert!(LinkProfile::new(MAX_PEERS_LIMIT + 1, 0, 0x1000).is_err());
    }

    #[test]
    fn shares_backing_across_reservations_and_isolates_links() {
        let registry = registry();
        let peer0 = registry.reserve(1, 0).unwrap();
        let peer1 = registry.reserve(1, 1).unwrap();
        let other = registry.reserve(2, 0).unwrap();
        peer0
            .link()
            .backing()
            .unwrap()
            .write(0x40, 8, 0x1234)
            .unwrap();
        assert_eq!(
            peer1.link().backing().unwrap().read(0x40, 8).unwrap(),
            0x1234
        );
        assert_eq!(other.link().backing().unwrap().read(0x40, 8).unwrap(), 0);
        assert!(!Arc::ptr_eq(peer0.link(), other.link()));
    }

    #[test]
    fn recreating_a_dead_link_retires_the_old_backing() {
        let registry = registry();
        let peer = registry.reserve(1, 0).unwrap();
        let stale = peer.link().backing().unwrap();
        stale.write(0, 4, 0xa5a5_a5a5).unwrap();
        drop(peer);
        assert_eq!(stale.read(0, 4).unwrap(), 0);

        let recreated = registry.reserve(1, 0).unwrap();
        let fresh = recreated.link().backing().unwrap();
        assert!(!Arc::ptr_eq(&stale, &fresh));
        stale.write(0x40, 8, 0x1111_2222_3333_4444).unwrap();
        assert_eq!(fresh.read(0x40, 8).unwrap(), 0);
        fresh.write(0x80, 8, 0x5555_6666_7777_8888).unwrap();
        assert_eq!(stale.read(0x80, 8).unwrap(), 0);
    }

    #[test]
    fn retired_peer_blocks_reservation_until_the_link_is_destroyed() {
        let registry = registry();
        let peer0 = registry.reserve(1, 0).unwrap();
        let peer1 = registry.reserve(1, 1).unwrap();
        drop(peer0);
        assert_eq!(
            registry.reserve(1, 0).unwrap_err(),
            IvshmemError::PeerRetired { link: 1, peer: 0 }
        );
        // The surviving peer keeps the link alive.
        assert!(registry.reserve(1, 1).is_err());
        drop(peer1);
        let recreated = registry.reserve(1, 0).unwrap();
        assert_eq!(recreated.peer_id().value(), 0);
    }

    #[test]
    fn attach_toggles_the_peer_slot() {
        let registry = registry();
        let reservation = registry.reserve(1, 0).unwrap();
        let attachment = reservation.attach().unwrap();
        assert_eq!(
            reservation.attach().unwrap_err(),
            IvshmemError::PeerAlreadyAttached { link: 1, peer: 0 }
        );
        assert_eq!(attachment.peer_id().value(), 0);
        assert_eq!(attachment.link().id().value(), 1);
        drop(attachment);
        // Bundle retries reuse the same reservation.
        let retried = reservation.attach().unwrap();
        drop(retried);
    }

    #[test]
    fn reservation_drop_while_attached_stays_consistent() {
        let registry = registry();
        let attachment = {
            let reservation = registry.reserve(1, 0).unwrap();
            reservation.attach().unwrap()
        };
        // The dropped reservation retired the slot; the stale attachment must
        // not resurrect it, and the link stays alive through the attachment.
        assert_eq!(
            registry.reserve(1, 0).unwrap_err(),
            IvshmemError::PeerRetired { link: 1, peer: 0 }
        );
        drop(attachment);
        let recreated = registry.reserve(1, 0).unwrap();
        recreated.attach().unwrap();
    }

    #[test]
    fn many_links_can_coexist_in_one_registry() {
        let registry = registry();
        let mut links = Vec::new();
        for link_id in 0..8u32 {
            links.push(registry.reserve(link_id, 0).unwrap());
        }
        for (index, link) in links.iter().enumerate() {
            assert_eq!(link.link().id().value(), index as u32);
        }
    }

    #[test]
    fn routes_a_doorbell_to_the_target_and_not_to_others() {
        let (peer0, peer1) = attached_pair();
        let sink0 = EventRecorder::new();
        let sink1 = EventRecorder::new();
        let attachment0 = peer0.attach().unwrap();
        let attachment1 = peer1.attach().unwrap();
        attachment0.set_event_sink(sink0.clone()).unwrap();
        attachment1.set_event_sink(sink1.clone()).unwrap();

        let written = doorbell(1, 0);
        attachment0
            .link()
            .deliver_doorbell(attachment0.peer_id(), written);
        assert_eq!(
            sink1.events(),
            vec![DoorbellEvent::new(attachment0.peer_id(), written)]
        );
        assert!(sink0.events().is_empty());
    }

    #[test]
    fn repeated_doorbells_merge_into_one_pending_bit() {
        let (peer0, peer1) = attached_pair();
        let sink1 = Arc::new(RegisterSink(Mutex::new(IvshmemRegisters::new())));
        let attachment0 = peer0.attach().unwrap();
        let attachment1 = peer1.attach().unwrap();
        attachment1.set_event_sink(sink1.clone()).unwrap();

        let link = attachment0.link();
        link.deliver_doorbell(attachment0.peer_id(), doorbell(1, 0));
        link.deliver_doorbell(attachment0.peer_id(), doorbell(1, 0));
        assert_eq!(sink1.pending(), 1);

        // The guest clears via W1C and the next doorbell pends again.
        sink1
            .0
            .lock()
            .unwrap()
            .write(crate::ivshmem::EVENT_STATUS_OFFSET, 1)
            .unwrap();
        assert_eq!(sink1.pending(), 0);
        link.deliver_doorbell(attachment0.peer_id(), doorbell(1, 0));
        assert_eq!(sink1.pending(), 1);
    }

    #[test]
    fn inactive_targets_and_unsupported_vectors_are_ignored() {
        // Peer 0 stays reserved without attaching: it is the inactive target.
        let (_peer0, peer1) = attached_pair();
        let sink1 = EventRecorder::new();
        let attachment1 = peer1.attach().unwrap();
        attachment1.set_event_sink(sink1.clone()).unwrap();

        // Peer 0 is reserved but not attached: the doorbell is a no-op.
        attachment1
            .link()
            .deliver_doorbell(attachment1.peer_id(), doorbell(0, 0));
        // Vector 1 is not supported by the current profile: also a no-op.
        attachment1
            .link()
            .deliver_doorbell(attachment1.peer_id(), doorbell(1, 1));
        // An out-of-profile target is a no-op, not a device error.
        attachment1
            .link()
            .deliver_doorbell(attachment1.peer_id(), doorbell(9, 0));
        assert!(sink1.events().is_empty());
    }

    #[test]
    fn a_failing_sink_is_ignored_without_panicking() {
        let (peer0, _peer1) = attached_pair();
        let attachment0 = peer0.attach().unwrap();
        attachment0.set_event_sink(Arc::new(FailingSink)).unwrap();
        attachment0
            .link()
            .deliver_doorbell(attachment0.peer_id(), doorbell(0, 0));
    }

    #[test]
    fn a_detached_endpoint_stops_receiving_doorbells() {
        let (peer0, peer1) = attached_pair();
        let sink1 = EventRecorder::new();
        let attachment0 = peer0.attach().unwrap();
        let attachment1 = peer1.attach().unwrap();
        attachment1.set_event_sink(sink1.clone()).unwrap();
        drop(attachment1);

        attachment0
            .link()
            .deliver_doorbell(attachment0.peer_id(), doorbell(1, 0));
        assert!(sink1.events().is_empty());
    }

    #[test]
    fn a_stale_sink_does_not_survive_reattachment() {
        let registry = registry();
        let reservation = registry.reserve(1, 0).unwrap();
        let stale = reservation.attach().unwrap();
        let stale_sink = EventRecorder::new();
        stale.set_event_sink(stale_sink.clone()).unwrap();
        drop(stale);
        // Bundle retries attach a new generation and must start without the
        // previous attachment's sink.
        let fresh = reservation.attach().unwrap();
        fresh
            .link()
            .deliver_doorbell(PeerId::new(1), doorbell(0, 0));
        assert!(stale_sink.events().is_empty());
    }

    #[test]
    fn set_event_sink_requires_the_current_attachment_generation() {
        let registry = registry();
        let reservation = registry.reserve(1, 0).unwrap();
        let current = reservation.attach().unwrap();
        // A stale-generation handle cannot arise in safe code: attachments
        // are neither Clone nor re-attachable while held. This white-box
        // handle exercises the generation guard on purpose.
        let stale = PeerAttachment {
            link: Arc::clone(reservation.link()),
            peer: reservation.peer_id(),
            generation: current.generation.wrapping_sub(1),
        };
        let epoch_offset = reservation
            .link()
            .layout()
            .peer_epoch_offset(reservation.peer_id(), reservation.link().max_peers())
            .unwrap();
        let epoch = reservation
            .link()
            .backing()
            .unwrap()
            .read_u64_acquire(epoch_offset)
            .unwrap();
        assert_eq!(
            stale.set_event_sink(EventRecorder::new()),
            Err(IvshmemError::PeerNotReserved { link: 1, peer: 0 })
        );
        assert_eq!(
            stale.reset(),
            Err(IvshmemError::PeerNotReserved { link: 1, peer: 0 })
        );
        assert_eq!(
            reservation
                .link()
                .backing()
                .unwrap()
                .read_u64_acquire(epoch_offset)
                .unwrap(),
            epoch
        );
        current.set_event_sink(EventRecorder::new()).unwrap();
    }

    #[test]
    fn sinks_run_outside_the_peer_table_lock() {
        let registry = registry();
        let reservation = registry.reserve(1, 0).unwrap();
        let link = Arc::clone(reservation.link());
        let attachment = reservation.attach().unwrap();
        let probe = Arc::new(LockProbeSink {
            link: Arc::clone(&link),
            lock_observed: AtomicBool::new(false),
        });
        attachment.set_event_sink(probe.clone()).unwrap();
        link.deliver_doorbell(PeerId::new(1), doorbell(0, 0));
        assert!(!probe.lock_observed.load(Ordering::Relaxed));
    }

    #[test]
    fn reset_publishes_epoch_then_notifies_survivors_without_locks() {
        let (peer0, peer1) = attached_pair();
        let attachment0 = peer0.attach().unwrap();
        let attachment1 = peer1.attach().unwrap();
        let sink0 = EventRecorder::new();
        let sink1 = EventRecorder::new();
        attachment0.set_event_sink(sink0.clone()).unwrap();
        attachment1.set_event_sink(sink1.clone()).unwrap();
        let link = Arc::clone(attachment0.link());
        let epoch_offset = link
            .layout()
            .peer_epoch_offset(attachment0.peer_id(), link.max_peers())
            .unwrap();
        let initial_epoch = link
            .backing()
            .unwrap()
            .read_u64_acquire(epoch_offset)
            .unwrap();
        assert_ne!(initial_epoch, 0);
        let stale_payload = 0x1122_3344_5566_7788;
        link.backing()
            .unwrap()
            .write(0x1000, 8, stale_payload)
            .unwrap();

        attachment0.reset().unwrap();

        let current_epoch = link
            .backing()
            .unwrap()
            .read_u64_acquire(epoch_offset)
            .unwrap();
        assert_eq!(current_epoch, initial_epoch + 1);
        assert_eq!(
            link.backing().unwrap().read(0x1000, 8).unwrap(),
            stale_payload
        );
        assert_eq!(sink1.epochs(), vec![(attachment0.peer_id(), current_epoch)]);
        assert!(sink0.epochs().is_empty());
        assert_eq!(
            link.backing()
                .unwrap()
                .read(
                    link.layout()
                        .state_entry_offset(attachment0.peer_id(), link.max_peers())
                        .unwrap(),
                    4,
                )
                .unwrap(),
            0
        );
        assert!(!link.state.is_locked());
    }

    #[test]
    fn lifecycle_notifications_run_outside_the_peer_table_lock() {
        let (peer0, peer1) = attached_pair();
        let attachment0 = peer0.attach().unwrap();
        let attachment1 = peer1.attach().unwrap();
        let link = Arc::clone(attachment1.link());
        let offset = link
            .layout()
            .peer_epoch_offset(attachment0.peer_id(), link.max_peers())
            .unwrap();
        let probe = Arc::new(EpochPublishProbe {
            link: Arc::clone(&link),
            offset,
            published: AtomicBool::new(false),
            lock_observed: AtomicBool::new(false),
        });
        attachment1.set_event_sink(probe.clone()).unwrap();

        attachment0.reset().unwrap();

        assert!(probe.published.load(Ordering::Relaxed));
        assert!(!probe.lock_observed.load(Ordering::Relaxed));
    }

    #[test]
    fn detach_and_release_publish_epochs_to_remaining_sinks() {
        let (peer0, peer1) = attached_pair();
        let attachment0 = peer0.attach().unwrap();
        let attachment1 = peer1.attach().unwrap();
        let sink0 = EventRecorder::new();
        let sink1 = EventRecorder::new();
        attachment0.set_event_sink(sink0.clone()).unwrap();
        attachment1.set_event_sink(sink1.clone()).unwrap();
        let link = Arc::clone(attachment0.link());
        let peer1_id = attachment1.peer_id();
        let offset = link
            .layout()
            .peer_epoch_offset(peer1_id, link.max_peers())
            .unwrap();
        let initial = link.backing().unwrap().read_u64_acquire(offset).unwrap();
        link.publish_state(peer1_id, 0x55).unwrap();

        drop(attachment1);
        let after_detach = link.backing().unwrap().read_u64_acquire(offset).unwrap();
        assert_eq!(after_detach, initial + 1);
        assert_eq!(sink0.epochs(), vec![(peer1_id, after_detach)]);
        assert!(sink1.epochs().is_empty());
        assert_eq!(
            link.backing()
                .unwrap()
                .read(
                    link.layout()
                        .state_entry_offset(peer1_id, link.max_peers())
                        .unwrap(),
                    4,
                )
                .unwrap(),
            0
        );

        let registry = registry();
        let peer2 = registry.reserve(1, 0).unwrap();
        let peer3 = registry.reserve(1, 1).unwrap();
        let attachment2 = peer2.attach().unwrap();
        let attachment3 = peer3.attach().unwrap();
        let sink2 = EventRecorder::new();
        let sink3 = EventRecorder::new();
        attachment2.set_event_sink(sink2.clone()).unwrap();
        attachment3.set_event_sink(sink3.clone()).unwrap();
        let link = Arc::clone(attachment2.link());
        let offset = link
            .layout()
            .peer_epoch_offset(attachment3.peer_id(), link.max_peers())
            .unwrap();
        let initial = link.backing().unwrap().read_u64_acquire(offset).unwrap();
        link.publish_state(attachment3.peer_id(), 0x66).unwrap();

        drop(peer3);
        let after_release = link.backing().unwrap().read_u64_acquire(offset).unwrap();
        assert_eq!(after_release, initial + 1);
        assert_eq!(sink2.epochs(), vec![(attachment3.peer_id(), after_release)]);
        assert!(sink3.epochs().is_empty());
        assert_eq!(
            link.backing()
                .unwrap()
                .read(
                    link.layout()
                        .state_entry_offset(attachment3.peer_id(), link.max_peers())
                        .unwrap(),
                    4,
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn state_writes_publish_into_the_shared_state_table() {
        let (peer0, peer1) = attached_pair();
        let link0 = Arc::clone(peer0.link());
        let link1 = Arc::clone(peer1.link());

        link0.publish_state(PeerId::new(0), 0x0001_0002).unwrap();
        // The owning link sees the entry through its own backing, and the
        // other peer's link is the same object: one shared state table.
        assert_eq!(link0.backing().unwrap().read(0, 4).unwrap(), 0x0001_0002);
        assert_eq!(link1.backing().unwrap().read(0, 4).unwrap(), 0x0001_0002);
        // The second peer's entry stays untouched.
        assert_eq!(link1.backing().unwrap().read(4, 4).unwrap(), 0);

        // Reserved bytes inside the state-table page read zero even after
        // entries are published.
        assert_eq!(link0.backing().unwrap().read(0x8, 4).unwrap(), 0);
        assert_eq!(link0.backing().unwrap().read(0xffc, 4).unwrap(), 0);

        // Clearing zeroes the entry without touching the shared region.
        link0.clear_state(PeerId::new(0)).unwrap();
        assert_eq!(link0.backing().unwrap().read(0, 4).unwrap(), 0);
    }

    #[test]
    fn state_publish_rejects_unknown_peers() {
        let registry = registry();
        let reservation = registry.reserve(1, 0).unwrap();
        let link = Arc::clone(reservation.link());
        assert_eq!(
            link.publish_state(PeerId::new(2), 0),
            Err(IvshmemError::PeerOutOfProfile {
                peer: 2,
                max_peers: MAX_PEERS
            })
        );
    }

    #[test]
    fn link_profile_rejects_invalid_configurations() {
        assert!(LinkProfile::new(0, 0, 0x1000).is_err());
        assert!(LinkProfile::new(65, 0, 0x1000).is_err());
        assert!(LinkProfile::new(2, 0x800, 0x1000).is_err());
        assert!(LinkProfile::new(2, 0, 0).is_err());
        assert!(LinkProfile::new(2, 0, 0x800).is_err());
        // The baseline is a valid profile.
        assert_eq!(LinkProfile::baseline().max_peers(), 2);
        assert_eq!(LinkProfile::baseline().common_size(), 0);
        assert_eq!(LinkProfile::baseline().bar2_size(), SHARED_MEMORY_SIZE);
        assert_eq!(
            LinkProfile::baseline().bar2_cache_policy(),
            Bar2CachePolicy::NonCacheable
        );

        let write_back = LinkProfile::baseline()
            .with_bar2_cache_policy(Bar2CachePolicy::WriteBack)
            .unwrap();
        assert_eq!(write_back.bar2_cache_policy(), Bar2CachePolicy::WriteBack);
        assert!(
            LinkProfile::new(3, 0, 0x1000)
                .unwrap()
                .with_bar2_cache_policy(Bar2CachePolicy::WriteBack)
                .is_err()
        );
        assert!(
            LinkProfile::new(2, 0x1000, 0x1000)
                .unwrap()
                .with_bar2_cache_policy(Bar2CachePolicy::WriteBack)
                .is_err()
        );
    }

    #[test]
    fn profile_derives_the_bounded_32_mib_layout_and_rejects_oversize_or_overflow() {
        assert_eq!(
            LinkProfile::new(1, 0, 0x1000).unwrap().bar2_size(),
            SHARED_MEMORY_SIZE
        );
        assert_eq!(
            LinkProfile::new(3, 0, 0x5000).unwrap().bar2_size(),
            SHARED_MEMORY_SIZE
        );
        let row_fixture = LinkProfile::new(2, 0, 0x00c0_0000).unwrap();
        assert_eq!(row_fixture.bar2_size(), MAX_BAR2_SIZE);
        assert_eq!(
            LinkProfile::new(2, 0, 0x00ff_f000).unwrap().bar2_size(),
            MAX_BAR2_SIZE
        );

        let allocator = Arc::new(TestAllocator::default());
        let registry = IvshmemLinkRegistry::new(allocator.clone());
        assert_eq!(
            registry
                .reserve_with_profile(1, 2, row_fixture)
                .unwrap_err(),
            IvshmemError::PeerOutOfProfile {
                peer: 2,
                max_peers: 2
            }
        );
        assert!(LinkProfile::new(2, 0, 0x0100_0000).is_err());
        assert!(LinkProfile::new(2, u64::MAX - 0xfff, 0x1000).is_err());
        assert!(LinkProfile::new(2, 0, u64::MAX - 0xfff).is_err());
        assert!(allocator.allocated_sizes().is_empty());
    }

    #[test]
    fn failed_first_allocation_does_not_publish_the_requested_profile() {
        let allocator = Arc::new(TestAllocator::default());
        let registry = IvshmemLinkRegistry::new(allocator.clone());
        allocator.fail_next_allocation();

        assert!(matches!(
            registry.reserve_with_profile(1, 0, LinkProfile::new(1, 0, 0x1000).unwrap()),
            Err(IvshmemError::AllocationFailed { .. })
        ));

        let reservation = registry
            .reserve_with_profile(1, 0, LinkProfile::new(1, 0, 0x2000).unwrap())
            .unwrap();
        assert_eq!(reservation.link().profile().output_size(), 0x2000);
        assert!(allocator.allocated_sizes().contains(&SHARED_MEMORY_SIZE));
    }

    #[test]
    fn creation_and_reactivation_allocate_the_same_profile_size() {
        let allocator = Arc::new(TestAllocator::default());
        let registry = IvshmemLinkRegistry::new(allocator.clone());
        let profile = LinkProfile::new(2, 0, 0x7000).unwrap();

        let first = registry.reserve_with_profile(1, 0, profile).unwrap();
        assert_eq!(first.link().bar2_size(), profile.bar2_size());
        assert_eq!(first.link().layout().bar2_size(), profile.bar2_size());
        drop(first);

        let second = registry.reserve_with_profile(1, 0, profile).unwrap();
        assert_eq!(second.link().bar2_size(), profile.bar2_size());
        assert_eq!(second.link().layout().bar2_size(), profile.bar2_size());
        assert_eq!(
            allocator.allocated_sizes(),
            vec![profile.bar2_size(), profile.bar2_size()]
        );
    }

    #[test]
    fn reservations_must_declare_matching_profiles() {
        let registry = registry();
        let _peer0 = registry
            .reserve_with_profile(1, 0, LinkProfile::new(3, 0x1000, 0x2000).unwrap())
            .unwrap();
        // A different profile for the same link mismatches.
        let error = registry
            .reserve_with_profile(1, 1, LinkProfile::baseline())
            .unwrap_err();
        assert!(matches!(
            error,
            IvshmemError::LinkProfileMismatch { link: 1, .. }
        ));
        // The matching profile reserves normally.
        registry
            .reserve_with_profile(1, 1, LinkProfile::new(3, 0x1000, 0x2000).unwrap())
            .unwrap();

        let write_back = LinkProfile::baseline()
            .with_bar2_cache_policy(Bar2CachePolicy::WriteBack)
            .unwrap();
        let _wb_peer0 = registry.reserve_with_profile(2, 0, write_back).unwrap();
        let error = registry
            .reserve_with_profile(2, 1, LinkProfile::baseline())
            .unwrap_err();
        assert!(matches!(
            error,
            IvshmemError::LinkProfileMismatch { link: 2, .. }
        ));
        registry.reserve_with_profile(2, 1, write_back).unwrap();
    }

    #[test]
    fn generation_advances_when_the_lifecycle_ends() {
        let registry = registry();
        let reservation = registry
            .reserve_with_profile(1, 0, LinkProfile::baseline())
            .unwrap();
        let link = Arc::clone(reservation.link());
        assert_eq!(link.generation().value(), 0);
        let epoch_offset = link
            .layout()
            .peer_epoch_offset(reservation.peer_id(), link.max_peers())
            .unwrap();
        let initial_epoch = link
            .backing()
            .unwrap()
            .read_u64_acquire(epoch_offset)
            .unwrap();
        assert_ne!(initial_epoch, 0);
        drop(reservation);
        // The skeleton survives with an advanced generation.
        assert_eq!(link.generation().value(), 1);
        // Re-reserving lands in the fresh lifecycle.
        let reattached = registry
            .reserve_with_profile(1, 0, LinkProfile::baseline())
            .unwrap();
        assert_eq!(reattached.link().generation().value(), 1);
        // The rebuilt backing is zeroed: old state is structurally invalid.
        assert_eq!(reattached.link().backing().unwrap().read(0, 4).unwrap(), 0);
        // The host-owned epoch survives the zeroed backing and reactivation.
        assert!(
            reattached
                .link()
                .backing()
                .unwrap()
                .read_u64_acquire(epoch_offset)
                .unwrap()
                > initial_epoch
        );
    }

    #[test]
    fn three_peers_route_without_cross_talk() {
        let registry = registry();
        let profile = LinkProfile::new(3, 0, 0x2000).unwrap();
        let reservations = [
            registry.reserve_with_profile(1, 0, profile).unwrap(),
            registry.reserve_with_profile(1, 1, profile).unwrap(),
            registry.reserve_with_profile(1, 2, profile).unwrap(),
        ];
        let sinks = [
            EventRecorder::new(),
            EventRecorder::new(),
            EventRecorder::new(),
        ];
        let attachments = [
            reservations[0].attach().unwrap(),
            reservations[1].attach().unwrap(),
            reservations[2].attach().unwrap(),
        ];
        for (attachment, sink) in attachments.iter().zip(sinks.iter()) {
            attachment.set_event_sink(sink.clone()).unwrap();
        }

        // Peer 1 rings peer 2 only.
        attachments[1]
            .link()
            .deliver_doorbell(attachments[1].peer_id(), doorbell(2, 0));
        assert_eq!(sinks[2].events().len(), 1);
        assert!(sinks[0].events().is_empty());
        assert!(sinks[1].events().is_empty());

        // Peer 0 rings peer 0 (self) only.
        attachments[0]
            .link()
            .deliver_doorbell(attachments[0].peer_id(), doorbell(0, 0));
        assert_eq!(sinks[0].events().len(), 1);
        assert_eq!(sinks[2].events().len(), 1);
    }

    #[test]
    fn doorbell_diagnostics_log_once_per_interval() {
        let counter = AtomicU32::new(0);
        assert!(IgnoredDoorbellCounters::should_log(&counter));
        // Occurrences 2..1023 stay silent; 1024 logs again.
        for _ in 1..DOORBELL_DIAGNOSTIC_INTERVAL - 1 {
            assert!(!IgnoredDoorbellCounters::should_log(&counter));
        }
        assert!(IgnoredDoorbellCounters::should_log(&counter));
    }
}
