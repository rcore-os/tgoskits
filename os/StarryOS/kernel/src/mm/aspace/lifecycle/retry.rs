//! Per-MM notification admission; completion remains owned by CPU leases.

use core::time::Duration;

pub(super) struct LazyReclaimRetry {
    next_attempt: Duration,
    delay: Duration,
}

impl Default for LazyReclaimRetry {
    fn default() -> Self {
        Self {
            next_attempt: Duration::ZERO,
            delay: Duration::from_millis(10),
        }
    }
}

impl LazyReclaimRetry {
    /// Claim before sending, under the owner's IRQ-safe lock. A stalled sender
    /// cannot block later attempts forever, and duplicate lifecycle callbacks
    /// cannot bypass the same deadline. There are no retry-count exhaustion or
    /// completion shortcuts: the retained MM remains queued until CPU release.
    pub(super) fn claim(&mut self, now: Duration) -> bool {
        if now < self.next_attempt {
            return false;
        }
        self.next_attempt = now.saturating_add(self.delay);
        self.delay = (self.delay * 2).min(Duration::from_secs(1));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg_attr(axtest, axtest::axtest)]
    #[cfg_attr(not(axtest), test)]
    fn notification_backoff_bounds_traffic_without_abandoning_retries() {
        let mut retry = LazyReclaimRetry::default();
        let mut now = Duration::from_secs(5);
        assert!(
            retry.claim(now),
            "the first retirement notification is immediate"
        );
        // Repeated callbacks and worker passes must share admission. Advancing
        // the clock models a controller that never acknowledges any attempt.
        for interval in [10, 20, 40, 80, 160, 320, 640, 1000, 1000, 1000] {
            assert!(!retry.claim(now), "duplicate callback bypassed backoff");
            now += Duration::from_millis(interval);
            assert!(!retry.claim(now - Duration::from_nanos(1)));
            assert!(retry.claim(now), "a persistent fault must remain retryable");
        }
        // One broken MM must not postpone a newly retired MM's first attempt.
        assert!(LazyReclaimRetry::default().claim(now));
    }
}
