//! Hardware dispatch-credit ownership for one hctx.

use core::sync::atomic::{AtomicUsize, Ordering};

/// Driver-advertised descriptors that may be hardware-owned concurrently.
pub(super) struct HardwareCredits {
    limit: usize,
    in_use: AtomicUsize,
}

impl HardwareCredits {
    pub(super) const fn new(limit: usize) -> Option<Self> {
        if limit == 0 {
            return None;
        }
        Some(Self {
            limit,
            in_use: AtomicUsize::new(0),
        })
    }

    /// Reserves one descriptor before a request enters `InFlight`.
    pub(super) fn try_reserve(&self) -> Option<HardwareCreditReservation<'_>> {
        let mut observed = self.in_use.load(Ordering::Acquire);
        loop {
            if observed >= self.limit {
                return None;
            }
            match self.in_use.compare_exchange_weak(
                observed,
                observed + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    return Some(HardwareCreditReservation {
                        credits: self,
                        retained_by_hardware: false,
                    });
                }
                Err(actual) => observed = actual,
            }
        }
    }

    /// Releases one credit after terminal ownership returns from hardware.
    pub(super) fn release_inflight(&self) {
        let previous = self.in_use.fetch_sub(1, Ordering::AcqRel);
        assert!(previous != 0, "block hctx hardware credit underflowed");
    }

    pub(super) fn in_use(&self) -> usize {
        self.in_use.load(Ordering::Acquire)
    }
}

/// RAII rollback for a credit reserved before calling the portable driver.
pub(super) struct HardwareCreditReservation<'credits> {
    credits: &'credits HardwareCredits,
    retained_by_hardware: bool,
}

impl HardwareCreditReservation<'_> {
    /// Transfers the reserved credit to the accepted in-flight request.
    pub(super) fn retain_for_inflight(mut self) {
        self.retained_by_hardware = true;
    }
}

impl Drop for HardwareCreditReservation<'_> {
    fn drop(&mut self) {
        if !self.retained_by_hardware {
            self.credits.release_inflight();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn max_inflight_one_blocks_a_second_dispatch_until_completion() {
        let credits = HardwareCredits::new(1).unwrap();
        let first = credits.try_reserve().unwrap();
        first.retain_for_inflight();

        assert!(credits.try_reserve().is_none());

        credits.release_inflight();
        assert!(credits.try_reserve().is_some());
    }

    #[test]
    fn rejected_dispatch_automatically_returns_its_credit() {
        let credits = HardwareCredits::new(1).unwrap();
        let rejected = credits.try_reserve().unwrap();

        drop(rejected);

        assert!(credits.try_reserve().is_some());
    }
}
