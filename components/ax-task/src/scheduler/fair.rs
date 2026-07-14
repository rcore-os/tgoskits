//! EEVDF scheduling entity calculations.

use crate::{FairMode, Nice};

const BASE_WEIGHT: u64 = 1024;

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
            virtual_deadline: virtual_time.saturating_add(weighted_request),
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
            .saturating_add(weighted_delta(runtime_ns, self.nice.weight()));
        self.remaining_request_ns = self.remaining_request_ns.saturating_sub(runtime_ns);
        self.remaining_request_ns == 0
    }

    /// Clamps placement to the current runqueue virtual time.
    pub(crate) fn place_at_least(&mut self, virtual_time: u64) {
        if self.vruntime >= virtual_time {
            return;
        }
        let shift = virtual_time - self.vruntime;
        self.vruntime = virtual_time;
        self.virtual_deadline = self.virtual_deadline.saturating_add(shift);
    }

    /// Starts a new request after expiry or explicit yield.
    pub(crate) fn renew_request(&mut self, virtual_time: u64) {
        let request_start = self.vruntime.max(virtual_time);
        self.remaining_request_ns = self.service_request_ns;
        self.virtual_deadline = request_start
            .saturating_add(weighted_delta(self.service_request_ns, self.nice.weight()));
    }

    /// Reweights one active request without discarding its service history.
    pub(crate) fn reconfigure(mut self, nice: Nice, mode: FairMode, virtual_time: u64) -> Self {
        let nice = if mode == FairMode::Idle {
            Nice::LOWEST
        } else {
            nice
        };
        let old_weight = self.nice.weight();
        let new_weight = nice.weight();
        self.nice = nice;
        self.mode = mode;
        if old_weight == new_weight {
            return self;
        }

        let lag = virtual_time as i128 - self.vruntime as i128;
        let reweighted_lag = lag.saturating_mul(old_weight as i128) / new_weight as i128;
        self.vruntime = (virtual_time as i128 - reweighted_lag).clamp(0, u64::MAX as i128) as u64;
        self.virtual_deadline = self
            .vruntime
            .saturating_add(weighted_delta(self.remaining_request_ns, new_weight));
        self
    }

    /// Reports whether the active request has no service left.
    pub(crate) const fn request_exhausted(self) -> bool {
        self.remaining_request_ns == 0
    }

    /// Returns whether non-negative lag makes this entity eligible.
    pub const fn is_eligible(self, virtual_time: u64) -> bool {
        self.vruntime <= virtual_time
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
        .min(u64::MAX as u128) as u64
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
}
