//! EEVDF scheduling entity calculations.

use crate::{FairMode, Nice};

const BASE_WEIGHT: u64 = 1024;
const MAX_VIRTUAL_DELTA: u64 = i64::MAX as u64;

/// Returns the signed modular distance from `reference` to `value`.
///
/// Linux compares EEVDF virtual time as `(s64)(a - b)`. Keeping every active
/// request and lag within half of the `u64` timeline makes this ordering stable
/// across wrap without periodically rewriting every queued entity.
pub(crate) const fn virtual_delta(value: u64, reference: u64) -> i64 {
    value.wrapping_sub(reference) as i64
}

pub(crate) const fn virtual_before(value: u64, reference: u64) -> bool {
    virtual_delta(value, reference) < 0
}

pub(crate) const fn virtual_after(value: u64, reference: u64) -> bool {
    virtual_delta(value, reference) > 0
}

pub(crate) const fn virtual_min(lhs: u64, rhs: u64) -> u64 {
    if virtual_before(rhs, lhs) { rhs } else { lhs }
}

pub(crate) const fn virtual_max(lhs: u64, rhs: u64) -> u64 {
    if virtual_after(rhs, lhs) { rhs } else { lhs }
}

/// Per-thread EEVDF service and lag state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FairEntity {
    nice: Nice,
    mode: FairMode,
    vruntime: u64,
    service_request_ns: u64,
    remaining_request_ns: u64,
    virtual_deadline: u64,
}

impl FairEntity {
    #[cfg(test)]
    pub(crate) const fn test_state(
        nice: Nice,
        mode: FairMode,
        vruntime: u64,
        virtual_deadline: u64,
    ) -> Self {
        Self {
            nice,
            mode,
            vruntime,
            service_request_ns: 1,
            remaining_request_ns: 1,
            virtual_deadline,
        }
    }

    /// Creates a fair entity at a run queue's current virtual time.
    pub fn new(nice: Nice, mode: FairMode, request_ns: u64, virtual_time: u64) -> Self {
        let nice = if mode == FairMode::Idle {
            Nice::LOWEST
        } else {
            nice
        };
        let weighted_request = weighted_delta(request_ns, nice.weight());
        Self {
            nice,
            mode,
            vruntime: virtual_time,
            service_request_ns: request_ns,
            remaining_request_ns: request_ns,
            virtual_deadline: virtual_time.wrapping_add(weighted_request),
        }
    }

    /// Charges physical execution without moving the active request deadline.
    ///
    /// Returns `true` exactly when cumulative execution consumes the request.
    /// The owner run queue starts a new request only after schedule-out, so
    /// sub-slice timer samples cannot restart EEVDF's virtual deadline.
    pub fn charge(&mut self, runtime_ns: u64, _virtual_time: u64) -> bool {
        self.vruntime = self
            .vruntime
            .wrapping_add(weighted_delta(runtime_ns, self.nice.weight()));
        self.remaining_request_ns = self.remaining_request_ns.saturating_sub(runtime_ns);
        self.remaining_request_ns == 0
    }

    /// Clamps placement to the current runqueue virtual time.
    pub(crate) fn place_at_least(&mut self, virtual_time: u64) {
        if !virtual_before(self.vruntime, virtual_time) {
            return;
        }
        let shift = virtual_time.wrapping_sub(self.vruntime);
        self.vruntime = virtual_time;
        self.virtual_deadline = self.virtual_deadline.wrapping_add(shift);
    }

    /// Starts a new request after expiry or explicit yield.
    pub(crate) fn renew_request(&mut self, virtual_time: u64) {
        let request_start = virtual_max(self.vruntime, virtual_time);
        self.remaining_request_ns = self.service_request_ns;
        self.virtual_deadline =
            request_start.wrapping_add(weighted_delta(self.service_request_ns, self.nice.weight()));
    }

    /// Forfeits an eligible request when the thread explicitly yields.
    ///
    /// Moving to the active virtual deadline lets positive-lag peers advance
    /// the runqueue virtual time instead of allowing the yielding thread to
    /// anchor eligibility at its old vruntime. An already ineligible entity
    /// keeps its request because it has already yielded its execution position.
    pub(crate) fn yield_request(&mut self, virtual_time: u64) {
        if self.is_eligible(virtual_time) {
            self.vruntime = virtual_max(self.vruntime, self.virtual_deadline);
            self.renew_request(virtual_time);
        } else if self.request_exhausted() {
            self.renew_request(virtual_time);
        }
    }

    /// Reweights and rebases one active request without discarding its lag.
    pub(crate) fn reconfigure(
        mut self,
        nice: Nice,
        mode: FairMode,
        source_virtual_time: u64,
        destination_virtual_time: u64,
    ) -> Self {
        let nice = if mode == FairMode::Idle {
            Nice::LOWEST
        } else {
            nice
        };
        let old_weight = self.nice.weight();
        let new_weight = nice.weight();
        let lag = i128::from(virtual_delta(source_virtual_time, self.vruntime));
        let reweighted_lag = (lag * i128::from(old_weight) / i128::from(new_weight))
            .clamp(-i128::from(i64::MAX), i128::from(i64::MAX)) as i64;
        self.nice = nice;
        self.mode = mode;
        self.vruntime = destination_virtual_time.wrapping_sub(reweighted_lag as u64);
        self.virtual_deadline = self
            .vruntime
            .wrapping_add(weighted_delta(self.remaining_request_ns, new_weight));
        self
    }

    /// Reports whether the active request has no service left.
    pub(crate) const fn request_exhausted(self) -> bool {
        self.remaining_request_ns == 0
    }

    /// Returns whether non-negative lag makes this entity eligible.
    pub const fn is_eligible(self, virtual_time: u64) -> bool {
        !virtual_after(self.vruntime, virtual_time)
    }

    /// Returns the entity's nice value.
    pub const fn nice(self) -> Nice {
        self.nice
    }

    /// Returns normal, batch, or idle fair semantics.
    pub const fn mode(self) -> FairMode {
        self.mode
    }

    /// Returns accumulated weighted virtual runtime.
    pub const fn vruntime(self) -> u64 {
        self.vruntime
    }

    /// Returns the Linux-compatible load weight used for lag accounting.
    pub(crate) const fn weight(self) -> u32 {
        self.nice.weight()
    }

    /// Returns the EEVDF virtual deadline.
    pub const fn virtual_deadline(self) -> u64 {
        self.virtual_deadline
    }

    /// Returns the physical service request used for this EEVDF slice.
    pub const fn service_request_ns(self) -> u64 {
        self.service_request_ns
    }

    /// Returns physical service left in the active request.
    pub const fn remaining_request_ns(self) -> u64 {
        self.remaining_request_ns
    }
}

fn weighted_delta(runtime_ns: u64, weight: u32) -> u64 {
    ((runtime_ns as u128).saturating_mul(BASE_WEIGHT as u128) / weight as u128)
        .min(MAX_VIRTUAL_DELTA as u128) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn higher_weight_accumulates_less_vruntime() {
        let mut favored = FairEntity::new(Nice::new(-5).unwrap(), FairMode::Normal, 1_000, 0);
        let mut default = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 0);
        favored.charge(1_000, 0);
        default.charge(1_000, 0);
        assert!(favored.vruntime() < default.vruntime());
    }

    #[test]
    fn virtual_deadline_stays_fixed_until_the_service_request_finishes() {
        let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 10_000);
        let deadline = entity.virtual_deadline();

        entity.charge(250, 10_000);

        assert_eq!(entity.virtual_deadline(), deadline);
    }

    #[test]
    fn sched_idle_always_uses_the_lowest_fair_weight() {
        let entity = FairEntity::new(Nice::new(-20).unwrap(), FairMode::Idle, 1_000, 0);

        assert_eq!(entity.nice(), Nice::new(19).unwrap());
    }

    #[test]
    fn virtual_time_comparison_survives_wrap() {
        let before_wrap = u64::MAX - 10;
        let after_wrap = 20;

        assert!(virtual_before(before_wrap, after_wrap));
        assert!(virtual_after(after_wrap, before_wrap));
        assert_eq!(virtual_min(before_wrap, after_wrap), before_wrap);
        assert_eq!(virtual_max(before_wrap, after_wrap), after_wrap);
    }

    #[test]
    fn reconfigure_preserves_lag_across_virtual_time_wrap() {
        let entity =
            FairEntity::test_state(Nice::ZERO, FairMode::Normal, u64::MAX - 50, u64::MAX - 49);

        let reconfigured = entity.reconfigure(Nice::ZERO, FairMode::Normal, 20, 100);

        assert_eq!(reconfigured.vruntime(), 29);
        assert_eq!(reconfigured.virtual_deadline(), 30);
    }
}
