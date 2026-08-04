//! Single-owner scheduler placement state machine.

use crate::{CpuId, CpuSet, TaskError};

/// CPU eligibility and physical scheduler ownership for one thread.
#[derive(Debug)]
pub(in crate::system) struct ThreadPlacementState {
    pub(in crate::system) affinity: CpuSet,
    pub(in crate::system) affinity_generation: u64,
    physical: SchedulerPlacement,
    on_cpu: Option<CpuId>,
}

impl ThreadPlacementState {
    pub(super) const fn new(affinity: CpuSet) -> Self {
        Self {
            affinity,
            affinity_generation: 1,
            physical: SchedulerPlacement::detached(),
            on_cpu: None,
        }
    }

    pub(in crate::system) const fn queued_cpu(&self) -> Option<CpuId> {
        self.physical.queued_cpu()
    }

    pub(in crate::system) const fn running_cpu(&self) -> Option<CpuId> {
        self.physical.running_cpu()
    }

    pub(in crate::system) const fn on_cpu(&self) -> Option<CpuId> {
        self.on_cpu
    }

    pub(in crate::system) const fn migration_target(&self) -> Option<CpuId> {
        self.physical.migration_target()
    }

    pub(in crate::system) fn assigned_cpu(&self) -> Option<CpuId> {
        self.physical
            .running_cpu()
            .or_else(|| self.physical.queued_cpu())
            .or(self.on_cpu)
            .or_else(|| self.physical.migration_target())
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
        match (self.on_cpu, cpu) {
            (None, Some(cpu)) if self.physical.running_cpu() == Some(cpu) => {
                self.on_cpu = Some(cpu);
                Ok(())
            }
            (Some(current), Some(cpu)) if current == cpu => Ok(()),
            (Some(_), None) if self.physical.running_cpu().is_none() => {
                self.on_cpu = None;
                Ok(())
            }
            _ => Err(TaskError::InvalidConfiguration),
        }
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

    pub(in crate::system) fn detach_exiting(&mut self, cpu: CpuId) -> Result<(), TaskError> {
        if self.on_cpu != Some(cpu) || self.physical.running_cpu() != Some(cpu) {
            return Err(TaskError::InvalidConfiguration);
        }
        self.physical = SchedulerPlacement::Detached;
        Ok(())
    }

    #[cfg(test)]
    pub(in crate::system) fn inject_missing_on_cpu(&mut self) {
        self.on_cpu = None;
    }

    #[cfg(test)]
    pub(in crate::system) fn inject_exiting_on_cpu(&mut self, cpu: CpuId) {
        self.physical = SchedulerPlacement::Detached;
        self.on_cpu = Some(cpu);
    }
}

/// One thread's complete physical runqueue/CPU ownership.
///
/// The runqueue-side destination of one thread.
///
/// Linux keeps `on_rq`/task CPU placement separate from the `on_cpu` release
/// completed by `finish_task_switch()`. This enum likewise records only the
/// final runqueue or migration owner. [`ThreadPlacementState::on_cpu`] retains
/// the outgoing physical CPU until the CPU-owned switch handoff is consumed;
/// no `SwitchingOut` mirror is stored on the thread.
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
    /// Detached from the source and owned by a generation-checked inbox transfer.
    Migrating { target: CpuId },
}

impl SchedulerPlacement {
    const fn detached() -> Self {
        Self::Detached
    }

    const fn queued_cpu(self) -> Option<CpuId> {
        match self {
            Self::Queued { cpu, .. } => Some(cpu),
            Self::Detached | Self::Running { .. } | Self::Migrating { .. } => None,
        }
    }

    const fn running_cpu(self) -> Option<CpuId> {
        match self {
            Self::Running { cpu, .. } => Some(cpu),
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
            Self::Migrating { target } => Some(target),
            Self::Detached => None,
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
                Self::Queued {
                    migration_target, ..
                },
                None,
            ) => migration_target.map_or(Self::Detached, |target| Self::Migrating { target }),
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
            (Self::Migrating { target }, Some(cpu)) if target == cpu => Self::Running {
                cpu,
                migration_target: None,
            },
            (
                Self::Running {
                    cpu: running_cpu, ..
                },
                Some(cpu),
            ) if running_cpu == cpu => current,
            (
                Self::Running {
                    migration_target, ..
                },
                None,
            ) => migration_target.map_or(Self::Detached, |target| Self::Migrating { target }),
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
            (Self::Detached, None) => current,
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const CPU0: CpuId = CpuId::new(0);
    const CPU1: CpuId = CpuId::new(1);

    #[test]
    fn switching_out_keeps_physical_cpu_until_tail() {
        let mut placement = ThreadPlacementState::new(CpuSet::all(2));
        placement.set_running_cpu(Some(CPU0)).unwrap();
        placement.set_on_cpu(Some(CPU0)).unwrap();
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
        let mut placement = ThreadPlacementState::new(CpuSet::all(2));
        placement.set_running_cpu(Some(CPU0)).unwrap();
        placement.set_on_cpu(Some(CPU0)).unwrap();
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
        let mut placement = ThreadPlacementState::new(CpuSet::all(2));
        placement.set_running_cpu(Some(CPU0)).unwrap();
        placement.set_on_cpu(Some(CPU0)).unwrap();
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

    #[test]
    fn switch_destination_is_not_mirrored_in_thread_placement() {
        let mut placement = ThreadPlacementState::new(CpuSet::all(2));
        placement.set_running_cpu(Some(CPU0)).unwrap();
        placement.set_on_cpu(Some(CPU0)).unwrap();
        placement.set_running_cpu(None).unwrap();
        placement.set_queued_cpu(Some(CPU0)).unwrap();

        assert_eq!(
            placement.physical,
            SchedulerPlacement::Queued {
                cpu: CPU0,
                migration_target: None,
            },
            "the CPU switch handoff, not the thread placement, must own the outgoing-stack phase"
        );
        assert_eq!(placement.on_cpu(), Some(CPU0));
    }
}
