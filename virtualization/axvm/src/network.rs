//! Process-wide counters for the isolated guest Ethernet switch.

use core::sync::atomic::{AtomicU64, Ordering};

use axvm_net::{DropReason, ForwardKind, RouteDecision};

/// Snapshot of software-switch forwarding and security counters.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct VirtualSwitchMetrics {
    /// Guest transmit-queue notifications observed by the switch.
    pub tx_notifications: u64,
    /// Transmit notifications that did not expose a new descriptor.
    pub empty_tx_notifications: u64,
    /// Valid Ethernet frames drained from guest transmit queues.
    pub transmitted_frames: u64,
    /// Ethernet bytes represented by [`Self::transmitted_frames`].
    pub transmitted_bytes: u64,
    /// Frames resolved to one destination port.
    pub unicast_frames: u64,
    /// Frames resolved to the other ports in the ingress segment.
    pub multicast_frames: u64,
    /// Target-port delivery operations selected by routing policy.
    pub forwarding_attempts: u64,
    /// Frame copies written into a ready destination receive queue.
    pub forwarded_copies: u64,
    /// Frames rejected because the topology did not contain the ingress port.
    pub dropped_unknown_ingress: u64,
    /// Frames shorter than an Ethernet header.
    pub dropped_short_frames: u64,
    /// Frames whose source MAC did not match the ingress port.
    pub dropped_spoofed_sources: u64,
    /// Unicast frames with no destination in the ingress segment.
    pub dropped_unknown_unicast: u64,
    /// Unicast frames addressed back to their ingress port.
    pub dropped_reflected_unicast: u64,
    /// Frames discarded when a bounded route decision could not be allocated.
    pub dropped_resource_exhaustion: u64,
    /// Frames discarded because a usable topology snapshot could not be built.
    pub dropped_topology_frames: u64,
    /// Topology construction batches that failed before routing.
    pub topology_snapshot_errors: u64,
    /// VM device snapshots skipped because the VM was not available.
    pub skipped_unavailable_vm_snapshots: u64,
    /// Target copies discarded because the destination receive queue was not ready.
    pub dropped_unavailable_rx_buffer: u64,
    /// Target-port delivery operations that returned an error or violated an invariant.
    pub delivery_errors: u64,
}

/// Statistics only: these atomics neither publish nor guard switch state, so
/// every update and snapshot may use relaxed ordering.
#[derive(Default)]
struct VirtualSwitchCounters {
    tx_notifications: AtomicU64,
    empty_tx_notifications: AtomicU64,
    transmitted_frames: AtomicU64,
    transmitted_bytes: AtomicU64,
    unicast_frames: AtomicU64,
    multicast_frames: AtomicU64,
    forwarding_attempts: AtomicU64,
    forwarded_copies: AtomicU64,
    dropped_unknown_ingress: AtomicU64,
    dropped_short_frames: AtomicU64,
    dropped_spoofed_sources: AtomicU64,
    dropped_unknown_unicast: AtomicU64,
    dropped_reflected_unicast: AtomicU64,
    dropped_resource_exhaustion: AtomicU64,
    dropped_topology_frames: AtomicU64,
    topology_snapshot_errors: AtomicU64,
    skipped_unavailable_vm_snapshots: AtomicU64,
    dropped_unavailable_rx_buffer: AtomicU64,
    delivery_errors: AtomicU64,
}

static COUNTERS: VirtualSwitchCounters = VirtualSwitchCounters {
    tx_notifications: AtomicU64::new(0),
    empty_tx_notifications: AtomicU64::new(0),
    transmitted_frames: AtomicU64::new(0),
    transmitted_bytes: AtomicU64::new(0),
    unicast_frames: AtomicU64::new(0),
    multicast_frames: AtomicU64::new(0),
    forwarding_attempts: AtomicU64::new(0),
    forwarded_copies: AtomicU64::new(0),
    dropped_unknown_ingress: AtomicU64::new(0),
    dropped_short_frames: AtomicU64::new(0),
    dropped_spoofed_sources: AtomicU64::new(0),
    dropped_unknown_unicast: AtomicU64::new(0),
    dropped_reflected_unicast: AtomicU64::new(0),
    dropped_resource_exhaustion: AtomicU64::new(0),
    dropped_topology_frames: AtomicU64::new(0),
    topology_snapshot_errors: AtomicU64::new(0),
    skipped_unavailable_vm_snapshots: AtomicU64::new(0),
    dropped_unavailable_rx_buffer: AtomicU64::new(0),
    delivery_errors: AtomicU64::new(0),
};

pub(crate) fn record_tx_notification() -> u64 {
    COUNTERS.tx_notifications.fetch_add(1, Ordering::Relaxed) + 1
}

pub(crate) fn record_empty_tx_notification() -> u64 {
    COUNTERS
        .empty_tx_notifications
        .fetch_add(1, Ordering::Relaxed)
        + 1
}

pub(crate) fn record_transmission(frame_len: usize) -> u64 {
    let count = COUNTERS.transmitted_frames.fetch_add(1, Ordering::Relaxed) + 1;
    COUNTERS
        .transmitted_bytes
        .fetch_add(frame_len as u64, Ordering::Relaxed);
    count
}

pub(crate) fn record_route(decision: &RouteDecision) {
    match decision {
        RouteDecision::Forward { kind, targets } => {
            let counter = match kind {
                ForwardKind::Unicast => &COUNTERS.unicast_frames,
                ForwardKind::Multicast => &COUNTERS.multicast_frames,
            };
            counter.fetch_add(1, Ordering::Relaxed);
            COUNTERS
                .forwarding_attempts
                .fetch_add(targets.len() as u64, Ordering::Relaxed);
        }
        RouteDecision::Drop(reason) => {
            let counter = match reason {
                DropReason::UnknownIngress => &COUNTERS.dropped_unknown_ingress,
                DropReason::FrameTooShort => &COUNTERS.dropped_short_frames,
                DropReason::SpoofedSource => &COUNTERS.dropped_spoofed_sources,
                DropReason::UnknownUnicast => &COUNTERS.dropped_unknown_unicast,
                DropReason::ReflectedUnicast => &COUNTERS.dropped_reflected_unicast,
                DropReason::ResourceExhausted => &COUNTERS.dropped_resource_exhaustion,
            };
            counter.fetch_add(1, Ordering::Relaxed);
        }
    }
}

pub(crate) fn record_forwarded_copy() -> u64 {
    COUNTERS.forwarded_copies.fetch_add(1, Ordering::Relaxed) + 1
}

pub(crate) fn record_topology_failure(dropped_frames: usize) {
    COUNTERS
        .topology_snapshot_errors
        .fetch_add(1, Ordering::Relaxed);
    COUNTERS
        .dropped_topology_frames
        .fetch_add(dropped_frames as u64, Ordering::Relaxed);
}

pub(crate) fn record_skipped_vm_snapshots(count: usize) {
    COUNTERS
        .skipped_unavailable_vm_snapshots
        .fetch_add(count as u64, Ordering::Relaxed);
}

pub(crate) fn record_unavailable_rx_buffer() {
    COUNTERS
        .dropped_unavailable_rx_buffer
        .fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn record_delivery_error() {
    COUNTERS.delivery_errors.fetch_add(1, Ordering::Relaxed);
}

/// Returns one lock-free snapshot of switch counters.
pub fn virtual_switch_metrics() -> VirtualSwitchMetrics {
    VirtualSwitchMetrics {
        tx_notifications: COUNTERS.tx_notifications.load(Ordering::Relaxed),
        empty_tx_notifications: COUNTERS.empty_tx_notifications.load(Ordering::Relaxed),
        transmitted_frames: COUNTERS.transmitted_frames.load(Ordering::Relaxed),
        transmitted_bytes: COUNTERS.transmitted_bytes.load(Ordering::Relaxed),
        unicast_frames: COUNTERS.unicast_frames.load(Ordering::Relaxed),
        multicast_frames: COUNTERS.multicast_frames.load(Ordering::Relaxed),
        forwarding_attempts: COUNTERS.forwarding_attempts.load(Ordering::Relaxed),
        forwarded_copies: COUNTERS.forwarded_copies.load(Ordering::Relaxed),
        dropped_unknown_ingress: COUNTERS.dropped_unknown_ingress.load(Ordering::Relaxed),
        dropped_short_frames: COUNTERS.dropped_short_frames.load(Ordering::Relaxed),
        dropped_spoofed_sources: COUNTERS.dropped_spoofed_sources.load(Ordering::Relaxed),
        dropped_unknown_unicast: COUNTERS.dropped_unknown_unicast.load(Ordering::Relaxed),
        dropped_reflected_unicast: COUNTERS.dropped_reflected_unicast.load(Ordering::Relaxed),
        dropped_resource_exhaustion: COUNTERS.dropped_resource_exhaustion.load(Ordering::Relaxed),
        dropped_topology_frames: COUNTERS.dropped_topology_frames.load(Ordering::Relaxed),
        topology_snapshot_errors: COUNTERS.topology_snapshot_errors.load(Ordering::Relaxed),
        skipped_unavailable_vm_snapshots: COUNTERS
            .skipped_unavailable_vm_snapshots
            .load(Ordering::Relaxed),
        dropped_unavailable_rx_buffer: COUNTERS
            .dropped_unavailable_rx_buffer
            .load(Ordering::Relaxed),
        delivery_errors: COUNTERS.delivery_errors.load(Ordering::Relaxed),
    }
}

pub(crate) const fn should_log_progress(count: u64) -> bool {
    count != 0 && (count <= 10 || count.is_multiple_of(100))
}

#[cfg(test)]
mod tests {
    use super::should_log_progress;

    #[test]
    fn progress_logging_is_rate_limited() {
        assert!(should_log_progress(1));
        assert!(should_log_progress(2));
        assert!(should_log_progress(10));
        assert!(should_log_progress(100));
        assert!(should_log_progress(200));
        assert!(!should_log_progress(0));
        assert!(!should_log_progress(11));
        assert!(!should_log_progress(99));
    }
}
