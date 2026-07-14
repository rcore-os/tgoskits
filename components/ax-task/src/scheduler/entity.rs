//! Class-specific mutable state stored with each thread.

use crate::{DeadlineEntity, FairEntity, SchedulePolicy, SchedulingKey};

/// Mutable scheduler accounting owned by one thread record.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulingEntity {
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
    pub fn new(policy: SchedulePolicy, fair_slice_ns: u64, virtual_time: u64) -> Self {
        match policy {
            SchedulePolicy::Fair { nice, mode } => {
                Self::Fair(FairEntity::new(nice, mode, fair_slice_ns, virtual_time))
            }
            SchedulePolicy::Fifo { .. } => Self::Fifo,
            SchedulePolicy::RoundRobin { quantum_ns, .. } => Self::RoundRobin {
                remaining_quantum_ns: quantum_ns,
            },
            SchedulePolicy::Deadline(policy) => Self::Deadline(DeadlineEntity::new(policy)),
        }
    }

    /// Prepares class state after a block/wake cycle.
    pub fn reset_after_wake(&mut self, policy: SchedulePolicy) {
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

    /// Charges one dispatch and reports whether its class slice expired.
    pub fn charge(&mut self, runtime_ns: u64, virtual_time: u64, reclaimed_ns: u64) -> bool {
        match self {
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
                (!entity.is_throttled()).then(|| entity.absolute_deadline_ns())
            }
            _ => None,
        }
    }

    /// Returns the EEVDF entity when this is a fair thread.
    pub const fn fair(self) -> Option<FairEntity> {
        match self {
            Self::Fair(entity) => Some(entity),
            _ => None,
        }
    }

    /// Returns the CBS entity when this is a Deadline thread.
    pub const fn deadline(self) -> Option<DeadlineEntity> {
        match self {
            Self::Deadline(entity) => Some(entity),
            _ => None,
        }
    }

    /// Reports whether this accounting representation matches a policy class.
    pub const fn matches_policy(self, policy: SchedulePolicy) -> bool {
        matches!(
            (self, policy),
            (Self::Fair(_), SchedulePolicy::Fair { .. })
                | (Self::Fifo, SchedulePolicy::Fifo { .. })
                | (Self::RoundRobin { .. }, SchedulePolicy::RoundRobin { .. })
                | (Self::Deadline(_), SchedulePolicy::Deadline(_))
        )
    }

    /// Reports whether a round-robin dispatch consumed its complete quantum.
    pub const fn round_robin_quantum_expired(self) -> bool {
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
    pub const fn is_deadline_throttled(self) -> bool {
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

    /// Makes an exhausted Deadline entity runnable for a PI-critical unlock path.
    pub(crate) fn enter_pi_critical_rescue(&mut self) {
        if let Self::Deadline(entity) = self {
            entity.enter_pi_critical_rescue();
        }
    }

    /// Restores CBS throttling after the last contended PI edge disappears.
    pub(crate) fn leave_pi_critical_rescue(&mut self) {
        if let Self::Deadline(entity) = self {
            entity.leave_pi_critical_rescue();
        }
    }

    /// Builds a scheduler urgency key from mutable class state.
    ///
    /// Deadline ordering uses the active absolute scheduling deadline rather
    /// than the relative policy value shared by every job.
    pub const fn scheduling_key(self, policy: SchedulePolicy, sequence: u64) -> SchedulingKey {
        match self {
            Self::Deadline(deadline) => deadline.scheduling_key(sequence),
            _ => policy.scheduling_key(sequence),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeadlineFlags, DeadlinePolicy, FairMode, Nice};

    #[test]
    fn fair_service_request_expires_after_cumulative_small_charges() {
        let policy = SchedulePolicy::fair(Nice::ZERO, FairMode::Normal);
        let mut entity = SchedulingEntity::new(policy, 100, 0);

        assert!(!entity.charge(40, 0, 0));
        assert!(!entity.charge(40, 0, 0));
        assert!(entity.charge(20, 0, 0));
    }

    #[test]
    fn deadline_urgency_uses_the_active_absolute_deadline() {
        let policy =
            SchedulePolicy::deadline(DeadlinePolicy::new(1, 10, 20, DeadlineFlags::NONE).unwrap());
        let mut earlier = SchedulingEntity::new(policy, 1, 0);
        let mut later = SchedulingEntity::new(policy, 1, 0);
        earlier.activate_deadline(100);
        later.activate_deadline(200);

        assert!(earlier.scheduling_key(policy, 2) < later.scheduling_key(policy, 1));
    }
}
