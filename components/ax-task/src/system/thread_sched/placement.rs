//! Single-owner scheduler placement state machine.

use crate::{CpuId, CpuSet, TaskError};

/// CPU eligibility and physical scheduler ownership for one thread.
#[derive(Debug)]
pub(in crate::system) struct ThreadPlacementState {
    pub(in crate::system) affinity: CpuSet,
    pub(in crate::system) affinity_generation: u64,
    physical: SchedulerPlacement,
}

impl ThreadPlacementState {
    pub(super) const fn new(affinity: CpuSet) -> Self {
        Self {
            affinity,
            affinity_generation: 1,
            physical: SchedulerPlacement::detached(),
        }
    }

    pub(in crate::system) const fn queued_cpu(&self) -> Option<CpuId> {
        self.physical.queued_cpu()
    }

    pub(in crate::system) const fn running_cpu(&self) -> Option<CpuId> {
        self.physical.running_cpu()
    }

    pub(in crate::system) const fn on_cpu(&self) -> Option<CpuId> {
        self.physical.on_cpu()
    }

    pub(in crate::system) const fn migration_target(&self) -> Option<CpuId> {
        self.physical.migration_target()
    }

    pub(in crate::system) fn assigned_cpu(&self) -> Option<CpuId> {
        self.physical.assigned_cpu()
    }

    pub(in crate::system) fn set_queued_cpu(
        &mut self,
        cpu: Option<CpuId>,
    ) -> Result<(), TaskError> {
        self.physical.set_queued_cpu(cpu)
    }

    pub(in crate::system) fn set_running_cpu(
        &mut self,
        cpu: Option<CpuId>,
    ) -> Result<(), TaskError> {
        self.physical.set_running_cpu(cpu)
    }

    pub(in crate::system) fn set_on_cpu(&mut self, cpu: Option<CpuId>) -> Result<(), TaskError> {
        self.physical.set_on_cpu(cpu)
    }

    pub(in crate::system) fn set_migration_target(
        &mut self,
        target: Option<CpuId>,
    ) -> Result<(), TaskError> {
        self.physical.set_migration_target(target)
    }

    pub(in crate::system) fn begin_queued_migration(
        &mut self,
        source: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        self.physical.begin_queued_migration(source, target)
    }

    pub(in crate::system) fn rollback_queued_migration(
        &mut self,
        source: CpuId,
        target: CpuId,
    ) -> Result<(), TaskError> {
        self.physical.rollback_queued_migration(source, target)
    }

    pub(in crate::system) fn mark_exited_awaiting_tail(
        &mut self,
        cpu: CpuId,
    ) -> Result<(), TaskError> {
        self.physical.mark_exited_awaiting_tail(cpu)
    }

    #[cfg(test)]
    pub(in crate::system) fn inject_detached(&mut self) {
        self.physical.inject_detached();
    }

    #[cfg(test)]
    pub(in crate::system) fn inject_exited_awaiting_tail(&mut self, cpu: CpuId) {
        self.physical.inject_exited_awaiting_tail(cpu);
    }
}

/// Destination committed while the outgoing stack is still physically active.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SwitchDestination {
    Detached,
    Queued(CpuId),
    Migrating(CpuId),
}

impl SwitchDestination {
    const fn into_placement(self) -> SchedulerPlacement {
        match self {
            Self::Detached => SchedulerPlacement::Detached,
            Self::Queued(cpu) => SchedulerPlacement::Queued {
                cpu,
                migration_target: None,
            },
            Self::Migrating(target) => SchedulerPlacement::Migrating { target },
        }
    }
}

/// One thread's complete physical runqueue/CPU ownership.
///
/// The former independent `queued_cpu`, `running_cpu`, `on_cpu`, and
/// `migration_target` fields admitted contradictory combinations. This enum
/// makes the Linux `finish_task_switch()` boundary explicit: a thread may have
/// a committed post-switch destination while its outgoing stack remains active,
/// but it cannot be independently queued and running on unrelated CPUs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchedulerPlacement {
    /// Not queued, executing, or in flight between owners.
    Detached,
    /// Physically linked on one owner runqueue.
    Queued {
        cpu: CpuId,
        migration_target: Option<CpuId>,
    },
    /// Current on one CPU, optionally with a pending affinity migration.
    Running {
        cpu: CpuId,
        migration_target: Option<CpuId>,
    },
    /// Scheduling state is committed, but the outgoing stack is still active.
    SwitchingOut {
        cpu: CpuId,
        destination: SwitchDestination,
    },
    /// Detached from the source and owned by a generation-checked inbox transfer.
    Migrating { target: CpuId },
    /// Exit is published, but switch tail has not cleared physical CPU ownership.
    ExitedAwaitingTail { cpu: CpuId },
}

impl SchedulerPlacement {
    const fn detached() -> Self {
        Self::Detached
    }

    const fn queued_cpu(self) -> Option<CpuId> {
        match self {
            Self::Queued { cpu, .. }
            | Self::SwitchingOut {
                destination: SwitchDestination::Queued(cpu),
                ..
            } => Some(cpu),
            Self::Detached
            | Self::Running { .. }
            | Self::SwitchingOut { .. }
            | Self::Migrating { .. }
            | Self::ExitedAwaitingTail { .. } => None,
        }
    }

    const fn running_cpu(self) -> Option<CpuId> {
        match self {
            Self::Running { cpu, .. } => Some(cpu),
            Self::Detached
            | Self::Queued { .. }
            | Self::SwitchingOut { .. }
            | Self::Migrating { .. }
            | Self::ExitedAwaitingTail { .. } => None,
        }
    }

    const fn on_cpu(self) -> Option<CpuId> {
        match self {
            Self::Running { cpu, .. }
            | Self::SwitchingOut { cpu, .. }
            | Self::ExitedAwaitingTail { cpu } => Some(cpu),
            Self::Detached | Self::Queued { .. } | Self::Migrating { .. } => None,
        }
    }

    const fn migration_target(self) -> Option<CpuId> {
        match self {
            Self::Queued {
                migration_target, ..
            }
            | Self::Running {
                migration_target, ..
            } => migration_target,
            Self::SwitchingOut {
                destination: SwitchDestination::Migrating(target),
                ..
            }
            | Self::Migrating { target } => Some(target),
            Self::Detached | Self::SwitchingOut { .. } | Self::ExitedAwaitingTail { .. } => None,
        }
    }

    /// Mirrors Linux `task_cpu()` ownership without confusing a future wake
    /// destination with a context that is still physically on its source CPU.
    fn assigned_cpu(self) -> Option<CpuId> {
        if let Some(cpu) = self.running_cpu() {
            Some(cpu)
        } else if let Some(cpu) = self.queued_cpu() {
            Some(cpu)
        } else if let Some(cpu) = self.on_cpu() {
            Some(cpu)
        } else {
            self.migration_target()
        }
    }

    fn set_queued_cpu(&mut self, cpu: Option<CpuId>) -> Result<(), TaskError> {
        let current = *self;
        *self = match (current, cpu) {
            (Self::Detached, Some(cpu)) => Self::Queued {
                cpu,
                migration_target: None,
            },
            (Self::Migrating { target }, Some(cpu)) if target == cpu => Self::Queued {
                cpu,
                migration_target: None,
            },
            (
                Self::SwitchingOut {
                    cpu: executing_cpu,
                    destination: SwitchDestination::Detached,
                },
                Some(cpu),
            ) if executing_cpu == cpu => Self::SwitchingOut {
                cpu,
                destination: SwitchDestination::Queued(cpu),
            },
            (
                Self::Queued {
                    migration_target, ..
                },
                None,
            ) => migration_target.map_or(Self::Detached, |target| Self::Migrating { target }),
            (
                Self::SwitchingOut {
                    cpu,
                    destination: SwitchDestination::Queued(_),
                },
                None,
            ) => Self::SwitchingOut {
                cpu,
                destination: SwitchDestination::Detached,
            },
            _ => return Err(TaskError::InvalidConfiguration),
        };
        Ok(())
    }

    fn set_running_cpu(&mut self, cpu: Option<CpuId>) -> Result<(), TaskError> {
        let current = *self;
        *self = match (current, cpu) {
            (Self::Detached, Some(cpu)) => Self::Running {
                cpu,
                migration_target: None,
            },
            (
                Self::Queued {
                    cpu: queued_cpu,
                    migration_target,
                },
                Some(cpu),
            ) if queued_cpu == cpu => Self::Running {
                cpu,
                migration_target,
            },
            (
                Self::SwitchingOut {
                    cpu: executing_cpu,
                    destination,
                },
                Some(cpu),
            ) if executing_cpu == cpu => Self::Running {
                cpu,
                migration_target: match destination {
                    SwitchDestination::Migrating(target) => Some(target),
                    SwitchDestination::Detached | SwitchDestination::Queued(_) => None,
                },
            },
            (
                Self::Running {
                    cpu: running_cpu, ..
                },
                Some(cpu),
            ) if running_cpu == cpu => current,
            (
                Self::Running {
                    cpu,
                    migration_target,
                },
                None,
            ) => Self::SwitchingOut {
                cpu,
                destination: migration_target
                    .map_or(SwitchDestination::Detached, SwitchDestination::Migrating),
            },
            (Self::SwitchingOut { .. }, None) => current,
            _ => return Err(TaskError::InvalidConfiguration),
        };
        Ok(())
    }

    fn set_on_cpu(&mut self, cpu: Option<CpuId>) -> Result<(), TaskError> {
        let current = *self;
        *self = match (current, cpu) {
            (
                Self::Running {
                    cpu: running_cpu, ..
                },
                Some(cpu),
            ) if running_cpu == cpu => current,
            (Self::SwitchingOut { destination, .. }, None) => destination.into_placement(),
            (Self::ExitedAwaitingTail { .. }, None) => Self::Detached,
            _ => return Err(TaskError::InvalidConfiguration),
        };
        Ok(())
    }

    fn set_migration_target(&mut self, target: Option<CpuId>) -> Result<(), TaskError> {
        let current = *self;
        *self = match (current, target) {
            (Self::Detached, Some(target)) => Self::Migrating { target },
            (
                Self::Queued {
                    cpu,
                    migration_target: _,
                },
                target,
            ) => Self::Queued {
                cpu,
                migration_target: target,
            },
            (
                Self::Running {
                    cpu,
                    migration_target: _,
                },
                target,
            ) => Self::Running {
                cpu,
                migration_target: target,
            },
            (Self::Migrating { .. }, Some(target)) => Self::Migrating { target },
            (Self::Migrating { .. }, None) => Self::Detached,
            (Self::SwitchingOut { cpu, .. }, Some(target)) => Self::SwitchingOut {
                cpu,
                destination: SwitchDestination::Migrating(target),
            },
            (
                Self::SwitchingOut {
                    cpu,
                    destination: SwitchDestination::Migrating(_),
                },
                None,
            ) => Self::SwitchingOut {
                cpu,
                destination: SwitchDestination::Detached,
            },
            (Self::Detached, None)
            | (
                Self::SwitchingOut {
                    destination: SwitchDestination::Detached | SwitchDestination::Queued(_),
                    ..
                },
                None,
            ) => current,
            (Self::ExitedAwaitingTail { .. }, _) => {
                return Err(TaskError::InvalidConfiguration);
            }
        };
        Ok(())
    }

    /// Atomically transfers logical ownership from the source runqueue to a
    /// not-yet-consumed migration carrier.
    fn begin_queued_migration(&mut self, source: CpuId, target: CpuId) -> Result<(), TaskError> {
        match *self {
            Self::Queued {
                cpu,
                migration_target: None,
            } if cpu == source => {
                *self = Self::Migrating { target };
                Ok(())
            }
            _ => Err(TaskError::InvalidConfiguration),
        }
    }

    /// Restores source ownership when the migration carrier was not published.
    fn rollback_queued_migration(&mut self, source: CpuId, target: CpuId) -> Result<(), TaskError> {
        match *self {
            Self::Queued {
                cpu,
                migration_target: None,
            } if cpu == source => Ok(()),
            Self::Migrating {
                target: migration_target,
            } if migration_target == target => {
                *self = Self::Queued {
                    cpu: source,
                    migration_target: None,
                };
                Ok(())
            }
            _ => Err(TaskError::InvalidConfiguration),
        }
    }

    fn mark_exited_awaiting_tail(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        match self {
            Self::Running {
                cpu: running_cpu, ..
            }
            | Self::SwitchingOut {
                cpu: running_cpu, ..
            } if *running_cpu == cpu => {
                *self = Self::ExitedAwaitingTail { cpu };
                Ok(())
            }
            _ => Err(TaskError::InvalidConfiguration),
        }
    }

    #[cfg(test)]
    fn inject_detached(&mut self) {
        *self = Self::Detached;
    }

    #[cfg(test)]
    fn inject_exited_awaiting_tail(&mut self, cpu: CpuId) {
        *self = Self::ExitedAwaitingTail { cpu };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CPU0: CpuId = CpuId::new(0);
    const CPU1: CpuId = CpuId::new(1);

    #[test]
    fn switching_out_keeps_physical_cpu_until_tail() {
        let mut placement = SchedulerPlacement::detached();
        placement.set_running_cpu(Some(CPU0)).unwrap();
        placement.set_running_cpu(None).unwrap();
        placement.set_queued_cpu(Some(CPU0)).unwrap();

        assert_eq!(placement.queued_cpu(), Some(CPU0));
        assert_eq!(placement.running_cpu(), None);
        assert_eq!(placement.on_cpu(), Some(CPU0));

        placement.set_on_cpu(None).unwrap();
        assert_eq!(placement.queued_cpu(), Some(CPU0));
        assert_eq!(placement.on_cpu(), None);
    }

    #[test]
    fn migration_becomes_transfer_owned_only_after_tail() {
        let mut placement = SchedulerPlacement::detached();
        placement.set_running_cpu(Some(CPU0)).unwrap();
        placement.set_migration_target(Some(CPU1)).unwrap();
        placement.set_running_cpu(None).unwrap();

        assert_eq!(placement.on_cpu(), Some(CPU0));
        assert_eq!(placement.migration_target(), Some(CPU1));

        placement.set_on_cpu(None).unwrap();
        assert_eq!(placement.on_cpu(), None);
        assert_eq!(placement.migration_target(), Some(CPU1));
    }

    #[test]
    fn outgoing_reselection_cancels_switch_tail_ownership() {
        let mut placement = SchedulerPlacement::detached();
        placement.set_running_cpu(Some(CPU0)).unwrap();
        placement.set_running_cpu(None).unwrap();
        placement.set_queued_cpu(Some(CPU0)).unwrap();
        placement.set_queued_cpu(None).unwrap();
        placement.set_running_cpu(Some(CPU0)).unwrap();

        assert_eq!(placement.running_cpu(), Some(CPU0));
        assert_eq!(placement.on_cpu(), Some(CPU0));
    }

    #[test]
    fn unrelated_cpu_cannot_claim_a_queued_thread() {
        let mut placement = SchedulerPlacement::detached();
        placement.set_queued_cpu(Some(CPU0)).unwrap();

        assert_eq!(
            placement.set_running_cpu(Some(CPU1)),
            Err(TaskError::InvalidConfiguration)
        );
        assert_eq!(placement.queued_cpu(), Some(CPU0));
        assert_eq!(placement.running_cpu(), None);
    }
}
