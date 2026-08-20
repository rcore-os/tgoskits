//! Class-specific mutable state stored with each thread.

use alloc::boxed::Box;

use crate::{DeadlineEntity, DeadlineServer, FairEntity, SchedulePolicy, SchedulingUrgency};

/// Stable unique owner of one task's complete class accounting.
///
/// Linux embeds the Fair, RT, and Deadline entities in `task_struct`; changing
/// effective priority never transfers the configured-policy entity to a
/// second owner or copies it through `rq->curr`. This handle provides the same
/// stable-record ownership rule without intrusive self references: one box
/// moves between task control and the owner rq while the complete scheduler
/// state stays at one address.
#[derive(Debug)]
pub(crate) struct ActiveSchedulingState {
    record: Box<ActiveSchedulingRecord>,
}

/// Complete class accounting behind one stable ownership handle.
#[derive(Debug)]
struct ActiveSchedulingRecord {
    effective_policy: SchedulePolicy,
    base_entity: SchedulingEntity,
    inherited_entity: Option<SchedulingEntity>,
}

impl ActiveSchedulingState {
    pub(crate) fn new(policy: SchedulePolicy, entity: SchedulingEntity) -> Self {
        Self {
            record: Box::new(ActiveSchedulingRecord {
                effective_policy: policy,
                base_entity: entity,
                inherited_entity: None,
            }),
        }
    }

    pub(crate) fn policy(&self) -> SchedulePolicy {
        self.record.effective_policy
    }

    pub(crate) fn entity(&self) -> &SchedulingEntity {
        self.record
            .inherited_entity
            .as_ref()
            .unwrap_or(&self.record.base_entity)
    }

    pub(crate) fn entity_mut(&mut self) -> &mut SchedulingEntity {
        self.record
            .inherited_entity
            .as_mut()
            .unwrap_or(&mut self.record.base_entity)
    }

    pub(crate) fn base_entity(&self) -> &SchedulingEntity {
        &self.record.base_entity
    }

    pub(crate) fn base_entity_mut(&mut self) -> &mut SchedulingEntity {
        &mut self.record.base_entity
    }

    pub(crate) fn replace_base_entity(&mut self, entity: SchedulingEntity) {
        self.record.base_entity = entity;
    }

    pub(crate) fn uses_inherited_entity(&self) -> bool {
        self.record.inherited_entity.is_some()
    }

    /// Makes the configured-policy entity effective again after PI deboost.
    pub(crate) fn use_base_entity(&mut self, policy: SchedulePolicy) {
        self.record.inherited_entity = None;
        self.record.effective_policy = policy;
    }

    /// Changes only the effective policy/key while retaining base accounting.
    ///
    /// This is used for same-class PI. In particular, an RR task keeps its
    /// remaining quantum while inheriting an RT priority.
    pub(crate) fn use_base_entity_with_effective_policy(&mut self, policy: SchedulePolicy) {
        debug_assert!(self.record.inherited_entity.is_none());
        self.record.effective_policy = policy;
    }

    /// Installs the class-specific entity used by a cross-class PI boost.
    pub(crate) fn use_inherited_entity(
        &mut self,
        policy: SchedulePolicy,
        entity: SchedulingEntity,
    ) {
        self.record.inherited_entity = Some(entity);
        self.record.effective_policy = policy;
    }

    pub(crate) fn update_inherited_effective_policy(&mut self, policy: SchedulePolicy) {
        debug_assert!(self.record.inherited_entity.is_some());
        self.record.effective_policy = policy;
    }
}

/// Mutable scheduler accounting owned by one thread record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum SchedulingEntity {
    /// Runtime-owned CPU-stopper work has no budget accounting.
    KernelStop,
    /// EEVDF fair accounting.
    Fair(FairEntity),
    /// FIFO needs only queue ordering state.
    Fifo,
    /// Round-robin preserves remaining quantum across higher-priority preemption.
    RoundRobin {
        /// Remaining quantum in nanoseconds.
        remaining_quantum_ns: u64,
    },
    /// EDF and CBS Deadline accounting.
    Deadline(DeadlineEntity),
}

impl SchedulingEntity {
    /// Creates class-specific state for a base policy.
    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub fn new(policy: SchedulePolicy, fair_slice_ns: u64, virtual_time: u64) -> Self {
        Self::new_with_deadline_server(
            policy,
            fair_slice_ns,
            virtual_time,
            DeadlineServer::unbound(),
        )
    }

    pub(crate) fn new_with_deadline_server(
        policy: SchedulePolicy,
        fair_slice_ns: u64,
        virtual_time: u64,
        deadline_server: DeadlineServer,
    ) -> Self {
        match policy {
            SchedulePolicy::KernelStop => Self::KernelStop,
            SchedulePolicy::Fair { nice, mode } => {
                Self::Fair(FairEntity::new(nice, mode, fair_slice_ns, virtual_time))
            }
            SchedulePolicy::Fifo { .. } => Self::Fifo,
            SchedulePolicy::RoundRobin { quantum_ns, .. } => Self::RoundRobin {
                remaining_quantum_ns: quantum_ns,
            },
            SchedulePolicy::Deadline(policy) => {
                Self::Deadline(DeadlineEntity::from_task_server(policy, deadline_server))
            }
        }
    }

    pub(crate) fn capture_fair_sleep_lag(&mut self, virtual_time: u64, timing_granularity_ns: u64) {
        if let Self::Fair(entity) = self {
            entity.capture_sleep_lag(virtual_time, timing_granularity_ns);
        }
    }

    pub(crate) fn capture_fair_migration(&mut self, virtual_time: u64, timing_granularity_ns: u64) {
        if let Self::Fair(entity) = self {
            entity.capture_migration(virtual_time, timing_granularity_ns);
        }
    }

    pub(crate) fn cancel_fair_migration(&mut self) {
        if let Self::Fair(entity) = self {
            entity.cancel_migration();
        }
    }

    /// Charges one dispatch and reports whether its class slice expired.
    pub fn charge(&mut self, runtime_ns: u64, virtual_time: u64, reclaimed_ns: u64) -> bool {
        match self {
            Self::KernelStop => false,
            Self::Fair(entity) => entity.charge(runtime_ns, virtual_time),
            Self::Fifo => false,
            Self::RoundRobin {
                remaining_quantum_ns,
            } => {
                *remaining_quantum_ns = remaining_quantum_ns.saturating_sub(runtime_ns);
                *remaining_quantum_ns == 0
            }
            Self::Deadline(entity) => entity.charge(runtime_ns, reclaimed_ns),
        }
    }

    /// Returns an absolute Deadline key when this is a Deadline entity.
    pub fn activate_deadline(&mut self, now_ns: u64) -> Option<u64> {
        match self {
            Self::Deadline(entity) => {
                entity.activate(now_ns);
                if entity.is_throttled() {
                    None
                } else {
                    entity.absolute_deadline_ns()
                }
            }
            _ => None,
        }
    }

    /// Returns the EEVDF entity when this is a fair thread.
    pub const fn fair(&self) -> Option<FairEntity> {
        match self {
            Self::Fair(entity) => Some(*entity),
            _ => None,
        }
    }

    /// Returns the CBS entity when this is a Deadline thread.
    pub const fn deadline(&self) -> Option<&DeadlineEntity> {
        match self {
            Self::Deadline(entity) => Some(entity),
            _ => None,
        }
    }

    /// Returns Deadline flags owned by the executing task. PI donor flags are
    /// reservation parameters only and must not grant reclaim or redirect
    /// overrun notification.
    pub fn deadline_owner_flags(&self) -> crate::DeadlineFlags {
        match self {
            Self::Deadline(entity) => entity.owner_flags(),
            _ => crate::DeadlineFlags::NONE,
        }
    }

    /// Reports whether this accounting representation matches a policy class.
    #[cfg(any(test, all(axtest, feature = "axtest")))]
    pub const fn matches_policy(&self, policy: SchedulePolicy) -> bool {
        matches!(
            (self, policy),
            (Self::KernelStop, SchedulePolicy::KernelStop)
                | (Self::Fair(_), SchedulePolicy::Fair { .. })
                | (Self::Fifo, SchedulePolicy::Fifo { .. })
                | (Self::RoundRobin { .. }, SchedulePolicy::RoundRobin { .. })
                | (Self::Deadline(_), SchedulePolicy::Deadline(_))
        )
    }

    /// Reports whether a round-robin dispatch consumed its complete quantum.
    pub const fn round_robin_quantum_expired(&self) -> bool {
        matches!(
            self,
            Self::RoundRobin {
                remaining_quantum_ns: 0
            }
        )
    }

    /// Starts a fresh round-robin quantum after yield or expiration.
    pub fn reset_round_robin_quantum(&mut self, policy: SchedulePolicy) {
        if let (
            Self::RoundRobin {
                remaining_quantum_ns,
            },
            SchedulePolicy::RoundRobin { quantum_ns, .. },
        ) = (self, policy)
        {
            *remaining_quantum_ns = quantum_ns;
        }
    }

    /// Returns whether an exhausted Deadline entity is throttled.
    pub fn is_deadline_throttled(&self) -> bool {
        matches!(self, Self::Deadline(entity) if entity.is_throttled())
    }

    /// Ends the active Deadline job and keeps it throttled until replenishment.
    pub(crate) fn yield_deadline_job(&mut self) -> bool {
        let Self::Deadline(entity) = self else {
            return false;
        };
        entity.yield_job();
        true
    }

    /// Builds PI urgency without a thread or arrival tie-break.
    pub fn scheduling_urgency(&self, policy: SchedulePolicy) -> SchedulingUrgency {
        match self {
            Self::Deadline(deadline) => deadline.scheduling_urgency(),
            _ => policy.scheduling_urgency(),
        }
    }
}

#[cfg(any(test, all(axtest, feature = "axtest")))]
mod tests {
    use super::*;
    use crate::{DeadlineFlags, DeadlinePolicy, FairMode, Nice};

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn fair_service_request_expires_after_cumulative_small_charges() {
        let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let mut entity = SchedulingEntity::new(policy, 100, 0);

        assert!(!entity.charge(40, 0, 0));
        assert!(!entity.charge(40, 0, 0));
        assert!(entity.charge(20, 0, 0));
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn deadline_urgency_uses_the_active_absolute_deadline() {
        let policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 10, 20, DeadlineFlags::NONE).unwrap());
        let mut earlier = SchedulingEntity::new(policy, 1, 0);
        let mut later = SchedulingEntity::new(policy, 1, 0);
        earlier.activate_deadline(100);
        later.activate_deadline(200);

        assert!(earlier.scheduling_urgency(policy) < later.scheduling_urgency(policy));
    }

    #[cfg_attr(test, test)]
    #[cfg_attr(all(axtest, feature = "axtest"), axtest::axtest)]
    fn deadline_urgency_orders_across_linux_rq_clock_wrap() {
        let earlier_policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 4, 20, DeadlineFlags::NONE).unwrap());
        let later_policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 10, 20, DeadlineFlags::NONE).unwrap());
        let mut earlier = SchedulingEntity::new(earlier_policy, 1, 0);
        let mut later = SchedulingEntity::new(later_policy, 1, 0);
        let now = u64::MAX - 5;
        earlier.activate_deadline(now);
        later.activate_deadline(now);

        assert_eq!(
            earlier.deadline().unwrap().absolute_deadline_ns(),
            Some(u64::MAX - 1)
        );
        assert_eq!(later.deadline().unwrap().absolute_deadline_ns(), Some(4));
        assert!(
            earlier.scheduling_urgency(earlier_policy) < later.scheduling_urgency(later_policy)
        );
    }
}
