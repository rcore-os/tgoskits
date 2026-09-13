use core::sync::atomic::{AtomicU64, Ordering};

use crate::IVC_DEFAULT_FALLBACK_POLL_ROUNDS;

/// Tracks peer notification events observed by a guest.
///
/// The OS-specific IRQ handler should call [`record_peer_event`] on the shared
/// counter. A waiter observes counter changes first and falls back to bounded
/// polling when no new IRQ event has arrived.
pub struct IvcPeerEventWaiter<'a> {
    irq_enabled: bool,
    counter: &'a AtomicU64,
    observed_count: AtomicU64,
}

impl<'a> IvcPeerEventWaiter<'a> {
    /// Creates a waiter over a shared notification counter.
    pub fn new(irq_enabled: bool, counter: &'a AtomicU64) -> Self {
        Self {
            irq_enabled,
            counter,
            observed_count: AtomicU64::new(counter.load(Ordering::Acquire)),
        }
    }

    /// Returns whether the guest successfully enabled notify IRQ delivery.
    pub const fn irq_enabled(&self) -> bool {
        self.irq_enabled
    }

    /// Observes one peer event or performs bounded fallback polling.
    pub fn wait_for_peer_event(&self) {
        self.wait_for_peer_event_with_poll(IVC_DEFAULT_FALLBACK_POLL_ROUNDS);
    }

    /// Observes one peer event or performs bounded fallback polling.
    pub fn wait_for_peer_event_with_poll(&self, fallback_poll_rounds: usize) {
        if self.irq_enabled && self.observe_peer_event() {
            return;
        }
        fallback_poll(fallback_poll_rounds);
    }

    /// Returns whether a new IRQ event was observed since the last call.
    pub fn observe_peer_event(&self) -> bool {
        let observed = self.observed_count.load(Ordering::Acquire);
        let current = self.counter.load(Ordering::Acquire);
        if current == observed {
            return false;
        }
        self.observed_count.store(current, Ordering::Release);
        true
    }
}

/// Records one peer notification event from an OS IRQ handler.
pub fn record_peer_event(counter: &AtomicU64) {
    counter.fetch_add(1, Ordering::AcqRel);
}

/// Performs bounded fallback polling.
pub fn fallback_poll(rounds: usize) {
    for _ in 0..rounds {
        core::hint::spin_loop();
    }
}
