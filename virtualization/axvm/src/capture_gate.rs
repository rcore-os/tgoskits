//! Epoch-based writer admission for resettable lock-free trace buffers.

use core::sync::atomic::{AtomicUsize, Ordering};

const ENABLED_BIT: usize = 1;

pub(crate) struct CaptureGate {
    epoch: AtomicUsize,
    active_writers: AtomicUsize,
}

impl CaptureGate {
    pub(crate) const fn new() -> Self {
        Self {
            epoch: AtomicUsize::new(0),
            active_writers: AtomicUsize::new(0),
        }
    }

    pub(crate) fn start(&self) {
        let previous = self.epoch.fetch_add(1, Ordering::Release);
        debug_assert_eq!(previous & ENABLED_BIT, 0);
    }

    pub(crate) fn stop(&self) {
        let _ = self
            .epoch
            .try_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                (epoch & ENABLED_BIT != 0).then(|| epoch.wrapping_add(1))
            });
        while self.active_writers.load(Ordering::Acquire) != 0 {
            core::hint::spin_loop();
        }
    }

    pub(crate) fn try_enter(&self) -> Option<CaptureWriter<'_>> {
        let observed_epoch = self.observe_enabled_epoch()?;
        self.try_enter_observed(observed_epoch)
    }

    fn observe_enabled_epoch(&self) -> Option<usize> {
        let epoch = self.epoch.load(Ordering::Acquire);
        (epoch & ENABLED_BIT != 0).then_some(epoch)
    }

    fn try_enter_observed(&self, observed_epoch: usize) -> Option<CaptureWriter<'_>> {
        self.active_writers.fetch_add(1, Ordering::AcqRel);
        if self.epoch.load(Ordering::Acquire) != observed_epoch {
            self.active_writers.fetch_sub(1, Ordering::Release);
            return None;
        }
        Some(CaptureWriter { gate: self })
    }
}

pub(crate) struct CaptureWriter<'a> {
    gate: &'a CaptureGate,
}

impl Drop for CaptureWriter<'_> {
    fn drop(&mut self) {
        self.gate.active_writers.fetch_sub(1, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_observation_cannot_enter_a_new_capture_epoch() {
        let gate = CaptureGate::new();
        gate.start();
        let stale_epoch = gate.observe_enabled_epoch().unwrap();

        gate.stop();
        gate.start();

        assert!(gate.try_enter_observed(stale_epoch).is_none());
        assert_eq!(gate.active_writers.load(Ordering::Relaxed), 0);
        assert!(gate.try_enter().is_some());
    }
}
