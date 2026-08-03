//! EEVDF scheduling entity calculations.

use crate::{FairMode, Nice, TaskError};

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
    placement: FairPlacement,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum FairPlacement {
    Initial,
    Active,
    Sleeping {
        virtual_lag: i64,
    },
    Migrating {
        virtual_lag: i64,
        relative_deadline: u64,
    },
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
            placement: FairPlacement::Active,
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
            placement: FairPlacement::Initial,
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

    /// Saves the bounded virtual lag at the point this entity stops competing.
    ///
    /// Linux records `se->vlag` against `avg_vruntime()` before dequeue. The
    /// sleeping task must retain that service credit while it is absent from
    /// the weighted average; deriving lag at wake time would instead reward
    /// arbitrary sleep duration.
    pub(crate) fn capture_sleep_lag(&mut self, virtual_time: u64, timing_granularity_ns: u64) {
        let virtual_lag = self.bounded_virtual_lag(virtual_time, timing_granularity_ns);
        self.placement = FairPlacement::Sleeping { virtual_lag };
    }

    /// Saves lag and the active request deadline before changing runqueues.
    pub(crate) fn capture_migration(&mut self, virtual_time: u64, timing_granularity_ns: u64) {
        if matches!(self.placement, FairPlacement::Migrating { .. }) {
            return;
        }
        let virtual_lag = self.bounded_virtual_lag(virtual_time, timing_granularity_ns);
        self.placement = FairPlacement::Migrating {
            virtual_lag,
            relative_deadline: self.virtual_deadline.wrapping_sub(self.vruntime),
        };
    }

    /// Cancels an unpublished migration without changing the active request.
    pub(crate) fn cancel_migration(&mut self) {
        if matches!(self.placement, FairPlacement::Migrating { .. }) {
            self.placement = FairPlacement::Active;
        }
    }

    /// Places a newly runnable entity into the runqueue competition.
    ///
    /// Linux gives an initial entity half a service request so it joins peers
    /// near their average progress. A sleeping entity instead restores its
    /// saved lag and starts a full request. Adding either entity changes the
    /// weighted average, so inflate saved lag by `(W + w) / W` before insert.
    pub(crate) fn place_after_activation(
        &mut self,
        virtual_time: u64,
        runnable_weight: u64,
    ) -> Result<(), TaskError> {
        let (saved_lag, request_ns) = match self.placement {
            FairPlacement::Initial => (0, self.service_request_ns / 2),
            FairPlacement::Sleeping { virtual_lag } => (virtual_lag, self.service_request_ns),
            FairPlacement::Active | FairPlacement::Migrating { .. } => {
                return Err(TaskError::InvalidConfiguration);
            }
        };
        let placement_lag = self.inflated_placement_lag(saved_lag, runnable_weight);
        self.vruntime = virtual_time.wrapping_sub(placement_lag as u64);
        self.remaining_request_ns = request_ns;
        self.virtual_deadline = self
            .vruntime
            .wrapping_add(weighted_delta(request_ns, self.nice.weight()));
        self.placement = FairPlacement::Active;
        Ok(())
    }

    /// Restores state after an owner-to-owner transfer.
    ///
    /// A wake forwarded through another CPU retains sleep placement, while an
    /// already-runnable migration retains its active request. The entity state,
    /// rather than the transport message, owns this semantic distinction.
    pub(crate) fn place_after_transfer(
        &mut self,
        virtual_time: u64,
        runnable_weight: u64,
    ) -> Result<(), TaskError> {
        match self.placement {
            FairPlacement::Initial | FairPlacement::Sleeping { .. } => {
                self.place_after_activation(virtual_time, runnable_weight)
            }
            FairPlacement::Migrating {
                virtual_lag,
                relative_deadline,
            } => {
                let placement_lag = self.inflated_placement_lag(virtual_lag, runnable_weight);
                self.vruntime = virtual_time.wrapping_sub(placement_lag as u64);
                self.virtual_deadline = self.vruntime.wrapping_add(relative_deadline);
                self.placement = FairPlacement::Active;
                Ok(())
            }
            FairPlacement::Active => {
                self.place_at_least(virtual_time);
                Ok(())
            }
        }
    }

    fn bounded_virtual_lag(self, virtual_time: u64, timing_granularity_ns: u64) -> i64 {
        let limit = weighted_delta(
            self.service_request_ns
                .saturating_add(timing_granularity_ns),
            self.nice.weight(),
        )
        .min(i64::MAX as u64) as i64;
        virtual_delta(virtual_time, self.vruntime).clamp(-limit, limit)
    }

    fn inflated_placement_lag(self, saved_lag: i64, runnable_weight: u64) -> i64 {
        if saved_lag == 0 || runnable_weight == 0 {
            return 0;
        }
        let weight = u64::from(self.nice.weight());
        (i128::from(saved_lag).saturating_mul(i128::from(runnable_weight.saturating_add(weight)))
            / i128::from(runnable_weight))
        .clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
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

    /// Returns whether this entity owns the earlier EEVDF deadline.
    ///
    /// Linux v7.1 compares virtual deadlines on the modular virtual-time
    /// timeline and lets EEVDF eligibility and slice protection decide wakeup
    /// preemption. A legacy physical-time wakeup granularity must not be added
    /// to these weighted virtual deadlines.
    pub(crate) const fn deadline_precedes(self, current: Self) -> bool {
        virtual_before(self.virtual_deadline, current.virtual_deadline)
    }

    /// Returns the physical service request used for this EEVDF slice.
    pub const fn service_request_ns(self) -> u64 {
        self.service_request_ns
    }

    /// Returns physical service left in the active request.
    pub const fn remaining_request_ns(self) -> u64 {
        self.remaining_request_ns
    }

    #[cfg(test)]
    pub(crate) const fn saved_sleep_lag(self) -> Option<i64> {
        match self.placement {
            FairPlacement::Sleeping { virtual_lag } => Some(virtual_lag),
            FairPlacement::Initial | FairPlacement::Active | FairPlacement::Migrating { .. } => {
                None
            }
        }
    }

    #[cfg(test)]
    pub(crate) const fn migration_pending(self) -> bool {
        matches!(self.placement, FairPlacement::Migrating { .. })
    }

    #[cfg(test)]
    pub(crate) const fn saved_migration(self) -> Option<(i64, u64)> {
        match self.placement {
            FairPlacement::Migrating {
                virtual_lag,
                relative_deadline,
            } => Some((virtual_lag, relative_deadline)),
            FairPlacement::Initial | FairPlacement::Active | FairPlacement::Sleeping { .. } => None,
        }
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
    fn initial_entity_enters_competition_with_half_a_service_request() {
        let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 0);

        entity.place_after_activation(10_000, 0).unwrap();

        assert_eq!(entity.service_request_ns(), 1_000);
        assert_eq!(entity.remaining_request_ns(), 500);
        assert_eq!(entity.virtual_deadline(), 10_500);
    }

    #[test]
    fn initial_entity_transferred_to_another_owner_keeps_initial_placement() {
        let mut entity = FairEntity::new(Nice::ZERO, FairMode::Normal, 1_000, 0);

        entity.place_after_transfer(10_000, 0).unwrap();

        assert_eq!(entity.remaining_request_ns(), 500);
        assert_eq!(entity.virtual_deadline(), 10_500);
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

    #[test]
    fn wakeup_deadline_comparison_survives_virtual_time_wrap() {
        let woken =
            FairEntity::test_state(Nice::ZERO, FairMode::Normal, u64::MAX - 30, u64::MAX - 20);
        let current = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 0, 5);

        assert!(woken.deadline_precedes(current));
    }

    #[test]
    fn earlier_eevdf_deadline_is_not_hidden_by_legacy_wakeup_granularity() {
        let woken = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 1_000, 1_500);
        let current = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 2_000, 3_000);

        assert!(woken.deadline_precedes(current));
    }

    #[test]
    fn forwarded_wake_keeps_sleep_placement_instead_of_an_active_deadline() {
        let mut entity = FairEntity::test_state(Nice::ZERO, FairMode::Normal, 900, 950);
        entity.capture_sleep_lag(1_000, 1_000);

        entity
            .place_after_transfer(2_000, u64::from(Nice::ZERO.weight()))
            .unwrap();

        assert_eq!(
            (entity.vruntime(), entity.virtual_deadline()),
            (1_800, 1_801)
        );
        assert_eq!(entity.remaining_request_ns(), entity.service_request_ns());
    }
}
