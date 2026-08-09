//! Layer-2 virtual switch core (merged from axvirtio-switch).
//!
//! Pure forwarding decision: maps guest MAC to port, classifies Ethernet
//! destination, hands frame to target port's ingress. Knows nothing about
//! AxVM, IRQs, DMA, host queues or QEMU. Testable with fake sinks.

use alloc::{collections::BTreeMap, sync::Arc};
use core::sync::atomic::{AtomicU64, Ordering};

use ax_kspin::SpinNoIrq as Mutex;

/// Typed identity of one switch port.
///
/// Combining `vm_id`, `generation` and `device_index` distinguishes the same
/// VM's device across a reset (new generation) and leaves room for several NICs
/// per VM later. Ports are never referred to by a bare `usize` (design §3.1).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct SwitchPortId {
    pub vm_id: usize,
    pub generation: usize,
    pub device_index: u16,
}

impl SwitchPortId {
    pub const fn new(vm_id: usize, generation: usize, device_index: u16) -> Self {
        Self {
            vm_id,
            generation,
            device_index,
        }
    }
}

/// Capability the switch exercises on a registered port.
///
/// Implemented by the concrete per-port endpoint in the hypervisor glue; tests
/// supply fakes. The switch never reaches past this boundary, so the data plane
/// it drives (queues, IRQs, guest device) stays hidden from the core.
pub trait SwitchPort: Send + Sync {
    /// Stable port identity.
    fn id(&self) -> SwitchPortId;
    /// Guest MAC this port was registered with (the anti-spoof reference).
    fn guest_mac(&self) -> [u8; 6];
    /// Whether the port is still live for this generation. A deactivated port
    /// is removed from the table, but a stale `Arc` may still be held briefly
    /// by the uplink worker; this gate makes such references benign.
    fn is_active(&self) -> bool;
    /// Pushes a frame toward the guest RX queue of this port.
    ///
    /// Returns `false` when the port's bounded ingress is full or the port is
    /// no longer active, so the caller can count the drop without aborting the
    /// rest of a broadcast fan-out (design §5.3).
    fn deliver_ingress(&self, frame: &[u8]) -> bool;
    /// Schedules the consumer after a frame was accepted by its ingress queue.
    ///
    /// The switch invokes this only after releasing its registry lock. The
    /// concrete runtime may use it to wake a blocked VM and poll its RX queue.
    fn notify_ingress(&self);
}

/// Why a frame left the switch without being delivered or uplinked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchDropReason {
    /// Fewer bytes than a 14-byte Ethernet header.
    Undersize,
    /// Ethernet source MAC does not match the port it arrived on.
    SourceMacViolation,
    /// The source port had been unregistered before the frame was classified.
    InactiveGeneration,
    /// Host RX frame did not match any active port and unknown unicast is not
    /// flooded inbound (design §5.2).
    UnknownUnicast,
}

/// Aggregate counters kept by the switch itself (per-port counters live on the
/// endpoint). Counters are `Relaxed`: they are observability only and never
/// gate a forwarding decision (design §9.2).
#[derive(Debug, Default)]
pub struct SwitchStats {
    pub host_rx_packets: AtomicU64,
    pub host_tx_requested: AtomicU64,
    pub unknown_unicast_drop: AtomicU64,
    pub broadcast_copies: AtomicU64,
    pub multicast_copies: AtomicU64,
    pub local_unicast_forwarded: AtomicU64,
    pub source_mac_violation: AtomicU64,
    pub undersize_drop: AtomicU64,
    pub inactive_generation_drop: AtomicU64,
    pub duplicate_mac_rejected: AtomicU64,
}

impl SwitchStats {
    fn inc(&self, field: &AtomicU64) {
        field.fetch_add(1, Ordering::Relaxed);
    }
}

/// Both port indices behind one lock (design §7).
#[derive(Default)]
struct SwitchRegistry {
    by_id: BTreeMap<SwitchPortId, Arc<dyn SwitchPort>>,
    by_mac: BTreeMap<[u8; 6], SwitchPortId>,
}

/// The shared layer-2 switch.
pub struct VirtualSwitch {
    registry: Mutex<SwitchRegistry>,
    stats: SwitchStats,
}

impl VirtualSwitch {
    /// Creates an empty switch wrapped in the [`Arc`] callers share between the
    /// port registry, the uplink worker and every guest delivery worker.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            registry: Mutex::new(SwitchRegistry::default()),
            stats: SwitchStats::default(),
        })
    }

    /// Read-only access to the aggregate counters.
    pub fn stats(&self) -> &SwitchStats {
        &self.stats
    }

    /// Registers a new port and binds the returned registration to this switch.
    ///
    /// Rejects a duplicate [`SwitchPortId`] or a duplicate guest MAC without
    /// disturbing the existing table — two VMs must not silently share a MAC
    /// (design §4, §8.3). The registration is bound to the owning
    /// `Arc<VirtualSwitch>` so dropping it removes exactly this port.
    pub fn register_owned(
        self: &Arc<Self>,
        port: Arc<dyn SwitchPort>,
    ) -> Result<SwitchPortRegistration, SwitchError> {
        let id = port.id();
        let mac = port.guest_mac();
        let mut registry = self.registry.lock();
        if registry.by_id.contains_key(&id) {
            self.stats.inc(&self.stats.duplicate_mac_rejected);
            return Err(SwitchError::DuplicatePortId(id));
        }
        if registry.by_mac.contains_key(&mac) {
            self.stats.inc(&self.stats.duplicate_mac_rejected);
            return Err(SwitchError::DuplicateMac(mac));
        }
        registry.by_id.insert(id, port);
        registry.by_mac.insert(mac, id);
        drop(registry);
        Ok(SwitchPortRegistration {
            switch: Some(self.clone()),
            id,
        })
    }

    /// Removes a port from both indices. Idempotent: a second removal (e.g.
    /// explicit deactivate after RAII drop) is a no-op.
    pub fn unregister(&self, id: SwitchPortId) {
        let mut registry = self.registry.lock();
        let Some(port) = registry.by_id.remove(&id) else {
            return;
        };
        // Remove the MAC index only if it still points at this generation; a
        // newer generation must not lose its entry because of an old one.
        if registry.by_mac.get(&port.guest_mac()) == Some(&id) {
            registry.by_mac.remove(&port.guest_mac());
        }
    }

    /// Returns a snapshot of all active port identities, in stable order. The
    /// uplink worker uses this to round-robin egress queues without holding the
    /// table lock while draining (design §5.3).
    pub fn active_port_ids(&self) -> alloc::vec::Vec<SwitchPortId> {
        self.registry
            .lock()
            .by_id
            .iter()
            .filter(|(_, port)| port.is_active())
            .map(|(id, _)| *id)
            .collect()
    }

    /// Switches a frame that a guest transmitted on `src_id`.
    ///
    /// Performs the anti-spoof source-MAC check, classifies the destination and
    /// pushes the frame into every target port's ingress (outside the table
    /// lock). Returns whether the caller must additionally submit the frame to
    /// the host uplink (design §5.1 decision table).
    pub fn switch_from_port(&self, src_id: SwitchPortId, frame: &[u8]) -> EgressOutcome {
        let header = match ethernet_destination(frame) {
            Some(header) => header,
            None => {
                self.stats.inc(&self.stats.undersize_drop);
                return EgressOutcome::dropped(SwitchDropReason::Undersize);
            }
        };

        // Snapshot the decision under the table lock; deliver outside it.
        let decision = {
            let registry = self.registry.lock();
            let Some(src_port) = registry.by_id.get(&src_id) else {
                drop(registry);
                self.stats.inc(&self.stats.inactive_generation_drop);
                return EgressOutcome::dropped(SwitchDropReason::InactiveGeneration);
            };
            if !src_port.is_active() {
                drop(registry);
                self.stats.inc(&self.stats.inactive_generation_drop);
                return EgressOutcome::dropped(SwitchDropReason::InactiveGeneration);
            }
            // Anti-spoof: the Ethernet source must be the MAC this port owns.
            if header.src != src_port.guest_mac() {
                drop(registry);
                self.stats.inc(&self.stats.source_mac_violation);
                return EgressOutcome::dropped(SwitchDropReason::SourceMacViolation);
            }
            classify_destination(&header.dst, src_id, &registry)
        };

        for target in decision.local_targets.iter() {
            if target.deliver_ingress(frame) {
                target.notify_ingress();
                match header.class() {
                    DestinationClass::Broadcast => {
                        self.stats.inc(&self.stats.broadcast_copies);
                    }
                    DestinationClass::Multicast => {
                        self.stats.inc(&self.stats.multicast_copies);
                    }
                    DestinationClass::Unicast => {
                        self.stats.inc(&self.stats.local_unicast_forwarded);
                    }
                }
            }
        }
        if decision.uplink {
            self.stats.inc(&self.stats.host_tx_requested);
        }
        EgressOutcome::Forwarded {
            uplink: decision.uplink,
        }
    }

    /// Distributes a frame that arrived from the host uplink.
    ///
    /// Known unicast -> only the target port; broadcast/multicast -> every
    /// active port; unknown unicast -> dropped and counted (design §5.2).
    pub fn switch_from_uplink(&self, frame: &[u8]) {
        self.stats.inc(&self.stats.host_rx_packets);
        let header = match ethernet_destination(frame) {
            Some(header) => header,
            None => {
                self.stats.inc(&self.stats.undersize_drop);
                return;
            }
        };

        let targets: alloc::vec::Vec<Arc<dyn SwitchPort>> = {
            let registry = self.registry.lock();
            match header.class() {
                DestinationClass::Unicast => match registry.by_mac.get(&header.dst) {
                    Some(id) => registry.by_id.get(id).cloned().into_iter().collect(),
                    None => {
                        drop(registry);
                        self.stats.inc(&self.stats.unknown_unicast_drop);
                        return;
                    }
                },
                DestinationClass::Broadcast | DestinationClass::Multicast => registry
                    .by_id
                    .values()
                    .filter(|port| port.is_active())
                    .cloned()
                    .collect(),
            }
        };

        for target in targets {
            if target.deliver_ingress(frame) {
                target.notify_ingress();
                match header.class() {
                    DestinationClass::Broadcast => {
                        self.stats.inc(&self.stats.broadcast_copies);
                    }
                    DestinationClass::Multicast => {
                        self.stats.inc(&self.stats.multicast_copies);
                    }
                    DestinationClass::Unicast => {
                        self.stats.inc(&self.stats.local_unicast_forwarded);
                    }
                }
            }
        }
    }
}

/// Forwarding decision captured under the table lock.
struct Decision {
    local_targets: alloc::vec::Vec<Arc<dyn SwitchPort>>,
    uplink: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DestinationClass {
    Unicast,
    Multicast,
    Broadcast,
}

/// Parsed Ethernet header slices of an inbound frame.
struct EthernetView {
    dst: [u8; 6],
    src: [u8; 6],
}

impl EthernetView {
    fn class(&self) -> DestinationClass {
        if self.dst == [0xff; 6] {
            DestinationClass::Broadcast
        } else if self.dst[0] & 0x01 != 0 {
            DestinationClass::Multicast
        } else {
            DestinationClass::Unicast
        }
    }
}

const ETHERNET_HEADER_LEN: usize = 14;

/// Extracts destination/source MAC views from a frame, or `None` if it is
/// shorter than the 14-byte Ethernet header.
fn ethernet_destination(frame: &[u8]) -> Option<EthernetView> {
    if frame.len() < ETHERNET_HEADER_LEN {
        return None;
    }
    let mut dst = [0u8; 6];
    let mut src = [0u8; 6];
    dst.copy_from_slice(&frame[0..6]);
    src.copy_from_slice(&frame[6..12]);
    Some(EthernetView { dst, src })
}

/// Builds the forwarding decision for a guest-originated frame.
///
/// - Known unicast (and not the source itself): target port only, no uplink.
/// - Broadcast / multicast: every active port except the source, plus uplink.
/// - Unknown unicast: no local delivery, uplink only (do not flood locally).
fn classify_destination(
    dst: &[u8; 6],
    src_id: SwitchPortId,
    registry: &SwitchRegistry,
) -> Decision {
    let class = EthernetView {
        dst: *dst,
        src: [0u8; 6],
    }
    .class();
    match class {
        DestinationClass::Unicast => match registry.by_mac.get(dst) {
            Some(target_id) if *target_id != src_id => Decision {
                local_targets: registry.by_id.get(target_id).into_iter().cloned().collect(),
                uplink: false,
            },
            // Self-directed or unknown unicast: do not loop back to the source
            // and do not flood locally; let the host uplink handle it.
            _ => Decision {
                local_targets: alloc::vec::Vec::new(),
                uplink: true,
            },
        },
        DestinationClass::Broadcast | DestinationClass::Multicast => {
            let local_targets = registry
                .by_id
                .values()
                .filter(|port| port.is_active() && port.id() != src_id)
                .cloned()
                .collect();
            Decision {
                local_targets,
                uplink: true,
            }
        }
    }
}

/// Outcome of switching one guest-originated frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressOutcome {
    /// Local delivery completed; `uplink` says whether the caller must also
    /// transmit the frame on the host uplink.
    Forwarded { uplink: bool },
    /// The frame was dropped for the given reason.
    Dropped(SwitchDropReason),
}

impl EgressOutcome {
    fn dropped(reason: SwitchDropReason) -> Self {
        EgressOutcome::Dropped(reason)
    }
}

/// Structured registration / configuration error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SwitchError {
    DuplicatePortId(SwitchPortId),
    DuplicateMac([u8; 6]),
}

/// RAII capability for a registered port.
///
/// Dropping the registration removes the port from both switch indices, so a
/// device that is torn down (VM stop/reset/remove, failed prepare) cannot keep
/// receiving frames. The registration intentionally does not hold a strong
/// reference to the endpoint: the adapter owns the endpoint, the registration
/// only owns the *table entry* (design §3.1, §8).
pub struct SwitchPortRegistration {
    /// `None` after [`release`](Self::release) or `Drop` has removed the entry,
    /// so neither runs the removal twice.
    switch: Option<Arc<VirtualSwitch>>,
    id: SwitchPortId,
}

impl SwitchPortRegistration {
    /// The registered port identity.
    pub fn id(&self) -> SwitchPortId {
        self.id
    }

    /// Removes the port from the table now (otherwise `Drop` does it).
    pub fn release(mut self) {
        if let Some(switch) = self.switch.take() {
            switch.unregister(self.id);
        }
    }
}

impl Drop for SwitchPortRegistration {
    fn drop(&mut self) {
        if let Some(switch) = self.switch.take() {
            switch.unregister(self.id);
        }
    }
}

impl core::fmt::Debug for SwitchPortRegistration {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        // The switch handle is not debug-printable; expose only the identity,
        // which is what diagnostics and test assertions need.
        f.debug_struct("SwitchPortRegistration")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;

    /// A minimal in-test port that records every frame it receives.
    struct FakePort {
        id: SwitchPortId,
        mac: [u8; 6],
        active: AtomicBool,
        delivered: Mutex<alloc::vec::Vec<alloc::vec::Vec<u8>>>,
        accept: AtomicUsize,
        notifications: AtomicUsize,
    }

    impl FakePort {
        fn new(id: SwitchPortId, mac: [u8; 6]) -> Arc<Self> {
            Arc::new(Self {
                id,
                mac,
                active: AtomicBool::new(true),
                delivered: Mutex::new(alloc::vec::Vec::new()),
                accept: AtomicUsize::new(usize::MAX),
                notifications: AtomicUsize::new(0),
            })
        }

        fn set_active(&self, active: bool) {
            self.active.store(active, Ordering::Release);
        }

        fn set_capacity(&self, capacity: usize) {
            self.accept.store(capacity, Ordering::Release);
        }

        fn delivered(&self) -> alloc::vec::Vec<alloc::vec::Vec<u8>> {
            self.delivered.lock().clone()
        }

        fn notifications(&self) -> usize {
            self.notifications.load(Ordering::Acquire)
        }
    }

    impl SwitchPort for FakePort {
        fn id(&self) -> SwitchPortId {
            self.id
        }
        fn guest_mac(&self) -> [u8; 6] {
            self.mac
        }
        fn is_active(&self) -> bool {
            self.active.load(Ordering::Acquire)
        }
        fn deliver_ingress(&self, frame: &[u8]) -> bool {
            if !self.is_active() {
                return false;
            }
            let mut delivered = self.delivered.lock();
            if delivered.len() >= self.accept.load(Ordering::Acquire) {
                return false;
            }
            delivered.push(frame.to_vec());
            true
        }
        fn notify_ingress(&self) {
            self.notifications.fetch_add(1, Ordering::Release);
        }
    }

    fn frame(dst: [u8; 6], src: [u8; 6]) -> alloc::vec::Vec<u8> {
        let mut f = alloc::vec::Vec::with_capacity(14);
        f.extend_from_slice(&dst);
        f.extend_from_slice(&src);
        f.extend_from_slice(&[0x08, 0x00]); // ethertype placeholder
        f.extend_from_slice(b"payload");
        f
    }

    const MAC_A: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
    const MAC_B: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x57];
    const MAC_HOST: [u8; 6] = [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02];

    fn port_id(vm: usize) -> SwitchPortId {
        SwitchPortId::new(vm, 1, 0)
    }

    fn two_port_switch() -> (
        Arc<VirtualSwitch>,
        Arc<FakePort>,
        Arc<FakePort>,
        SwitchPortRegistration,
        SwitchPortRegistration,
    ) {
        let switch = VirtualSwitch::new();
        let a = FakePort::new(port_id(1), MAC_A);
        let b = FakePort::new(port_id(2), MAC_B);
        let ra = switch.register_owned(a.clone()).unwrap();
        let rb = switch.register_owned(b.clone()).unwrap();
        (switch, a, b, ra, rb)
    }

    #[test]
    fn registers_two_ports_and_rejects_duplicates() {
        let switch = VirtualSwitch::new();
        let a = FakePort::new(port_id(1), MAC_A);
        let b = FakePort::new(port_id(2), MAC_B);
        // Hold the registrations: dropping a `SwitchPortRegistration` removes the
        // port, so a temporary `is_ok()` would free the port before the
        // duplicate check below.
        let ra = switch.register_owned(a.clone()).unwrap();
        let rb = switch.register_owned(b.clone()).unwrap();

        // Duplicate MAC even with a different id is rejected.
        let a_dup = FakePort::new(SwitchPortId::new(3, 1, 0), MAC_A);
        assert_eq!(
            switch.register_owned(a_dup).unwrap_err(),
            SwitchError::DuplicateMac(MAC_A)
        );

        // Duplicate id even with a different MAC is rejected.
        let id_dup = FakePort::new(port_id(1), [0x52, 0x54, 0x00, 0x00, 0x00, 0x09]);
        assert_eq!(
            switch.register_owned(id_dup).unwrap_err(),
            SwitchError::DuplicatePortId(port_id(1))
        );

        drop(ra);
        drop(rb);
    }

    #[test]
    fn known_unicast_delivers_only_to_target_without_uplink() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        let outcome = switch.switch_from_port(port_id(1), &frame(MAC_B, MAC_A));
        assert_eq!(outcome, EgressOutcome::Forwarded { uplink: false });
        assert!(a.delivered().is_empty());
        assert_eq!(b.delivered().len(), 1);
        assert_eq!(a.notifications(), 0);
        assert_eq!(b.notifications(), 1);
    }

    #[test]
    fn broadcast_fans_out_to_other_ports_and_uplink() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        let outcome = switch.switch_from_port(port_id(1), &frame([0xff; 6], MAC_A));
        assert_eq!(outcome, EgressOutcome::Forwarded { uplink: true });
        assert!(a.delivered().is_empty()); // source excluded
        assert_eq!(b.delivered().len(), 1);
    }

    #[test]
    fn multicast_fans_out_to_other_ports_and_uplink() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        let mcast = [0x01, 0x00, 0x5e, 0x00, 0x00, 0x01];
        let outcome = switch.switch_from_port(port_id(2), &frame(mcast, MAC_B));
        assert_eq!(outcome, EgressOutcome::Forwarded { uplink: true });
        assert_eq!(a.delivered().len(), 1);
        assert!(b.delivered().is_empty()); // source excluded
    }

    #[test]
    fn unknown_unicast_is_uplinked_only() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        let outcome = switch.switch_from_port(port_id(1), &frame(MAC_HOST, MAC_A));
        assert_eq!(outcome, EgressOutcome::Forwarded { uplink: true });
        assert!(a.delivered().is_empty());
        assert!(b.delivered().is_empty());
    }

    #[test]
    fn source_mac_spoof_is_dropped() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        let outcome = switch.switch_from_port(port_id(1), &frame(MAC_B, MAC_B));
        assert_eq!(
            outcome,
            EgressOutcome::Dropped(SwitchDropReason::SourceMacViolation)
        );
        assert!(a.delivered().is_empty());
        assert!(b.delivered().is_empty());
        assert_eq!(
            switch.stats().source_mac_violation.load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn host_rx_known_unicast_targets_one_port() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        switch.switch_from_uplink(&frame(MAC_A, MAC_HOST));
        assert_eq!(a.delivered().len(), 1);
        assert!(b.delivered().is_empty());
    }

    #[test]
    fn host_rx_broadcast_fans_out_to_all() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        switch.switch_from_uplink(&frame([0xff; 6], MAC_HOST));
        assert_eq!(a.delivered().len(), 1);
        assert_eq!(b.delivered().len(), 1);
    }

    #[test]
    fn host_rx_unknown_unicast_is_dropped() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        switch.switch_from_uplink(&frame([0x52, 0x54, 0x00, 0x00, 0x00, 0x99], MAC_HOST));
        assert!(a.delivered().is_empty());
        assert!(b.delivered().is_empty());
        assert_eq!(
            switch.stats().unknown_unicast_drop.load(Ordering::Relaxed),
            1
        );
    }

    #[test]
    fn dropping_registration_removes_port_from_table() {
        let (switch, a, b, ra, _rb) = two_port_switch();
        // While registered, A receives host broadcasts.
        switch.switch_from_uplink(&frame([0xff; 6], MAC_HOST));
        assert_eq!(a.delivered().len(), 1);

        drop(ra);
        // After A's registration is dropped, only B receives the broadcast.
        switch.switch_from_uplink(&frame([0xff; 6], MAC_HOST));
        assert_eq!(a.delivered().len(), 1); // unchanged
        assert_eq!(b.delivered().len(), 2);
    }

    #[test]
    fn one_full_ingress_does_not_block_other_copies_or_uplink() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        a.set_capacity(0); // A's ingress rejects everything.
        let outcome = switch.switch_from_port(port_id(1), &frame([0xff; 6], MAC_A));
        // Broadcast still requests the uplink and still reaches B.
        assert_eq!(outcome, EgressOutcome::Forwarded { uplink: true });
        assert!(a.delivered().is_empty());
        assert_eq!(b.delivered().len(), 1);
    }

    #[test]
    fn inactive_port_is_skipped_during_fanout() {
        let (switch, a, b, _ra, _rb) = two_port_switch();
        b.set_active(false);
        let outcome = switch.switch_from_port(port_id(1), &frame([0xff; 6], MAC_A));
        assert_eq!(outcome, EgressOutcome::Forwarded { uplink: true });
        assert!(a.delivered().is_empty()); // source
        assert_eq!(b.delivered().len(), 0); // inactive
    }

    #[test]
    fn stale_generation_frame_is_dropped() {
        let switch = VirtualSwitch::new();
        let stale_id = SwitchPortId::new(1, 1, 0);
        // No port registered for this id.
        let outcome = switch.switch_from_port(stale_id, &frame(MAC_B, MAC_A));
        assert_eq!(
            outcome,
            EgressOutcome::Dropped(SwitchDropReason::InactiveGeneration)
        );
    }

    #[test]
    fn generation_reuse_after_unregister_succeeds() {
        let switch = VirtualSwitch::new();
        let mac = MAC_A;
        let old = FakePort::new(SwitchPortId::new(1, 1, 0), mac);
        let reg = switch.register_owned(old).unwrap();
        drop(reg);
        // A new generation may reuse the same MAC once the old port is gone.
        let new = FakePort::new(SwitchPortId::new(1, 2, 0), mac);
        assert!(switch.register_owned(new).is_ok());
    }
}
