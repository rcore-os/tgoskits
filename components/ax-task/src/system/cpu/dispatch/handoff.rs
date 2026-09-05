//! Move-only context-switch tail ownership.

use super::super::*;

/// State committed before an architecture switch and consumed by switch tail.
#[derive(Debug)]
pub(crate) struct SwitchHandoff {
    previous: PreviousSwitchOwnership,
    incoming: SchedulerThreadRef,
    incoming_policy: SchedulerPolicyRef,
    previous_disposition: PreviousSwitchDisposition,
    route: SwitchRoute,
}

/// Lifetime source for the outgoing task across the architecture switch.
#[derive(Debug)]
pub(crate) enum PreviousSwitchOwnership {
    /// The owner rq and its transferred lock baton retain the linked task.
    SchedulerOwned(SchedulerThreadRef),
    /// Exit and migration paths retain an independent strong reference.
    Retained(Arc<ThreadCore>),
}

impl PreviousSwitchOwnership {
    pub(crate) fn retained(core: Arc<ThreadCore>) -> Self {
        Self::Retained(core)
    }

    pub(crate) const fn scheduler_owned(core: SchedulerThreadRef) -> Self {
        Self::SchedulerOwned(core)
    }

    pub(crate) fn as_ref(&self) -> &ThreadCore {
        match self {
            Self::SchedulerOwned(core) => core.as_ref(),
            Self::Retained(core) => core.as_ref(),
        }
    }

    fn retained_arc(&self) -> Option<&Arc<ThreadCore>> {
        match self {
            Self::SchedulerOwned(_) => None,
            Self::Retained(core) => Some(core),
        }
    }

    const fn requires_rq_baton(&self) -> bool {
        matches!(self, Self::SchedulerOwned(_))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreviousSwitchDisposition {
    Live,
    Exited,
}

#[derive(Debug)]
enum SwitchRoute {
    Local { rq_baton: Option<RqSwitchBaton> },
    Migration(PreparedMigrationDelivery),
}

pub(crate) struct CompletedMigrationSwitchHandoff {
    pub(crate) incoming: SchedulerThreadRef,
    pub(crate) incoming_policy: SchedulerPolicyRef,
    pub(crate) migration: PreparedMigrationDelivery,
    pub(crate) reclaim_ready: bool,
    pub(crate) previous_exited: bool,
}

impl SwitchHandoff {
    pub(crate) fn prepared(
        previous: PreviousSwitchOwnership,
        incoming: SchedulerThreadRef,
        incoming_policy: SchedulerPolicyRef,
        previous_disposition: PreviousSwitchDisposition,
        migration: Option<PreparedMigrationDelivery>,
    ) -> Self {
        Self {
            previous,
            incoming,
            incoming_policy,
            previous_disposition,
            route: match migration {
                Some(migration) => SwitchRoute::Migration(migration),
                None => SwitchRoute::Local { rq_baton: None },
            },
        }
    }

    pub(crate) fn install_rq_baton(&mut self, baton: RqSwitchBaton) -> Result<(), TaskError> {
        match &mut self.route {
            SwitchRoute::Local { rq_baton } if rq_baton.is_none() => {
                *rq_baton = Some(baton);
                Ok(())
            }
            SwitchRoute::Local { .. } | SwitchRoute::Migration(_) => {
                Err(TaskError::InvalidConfiguration)
            }
        }
    }

    pub(crate) fn has_rq_baton(&self) -> bool {
        matches!(self.route, SwitchRoute::Local { rq_baton: Some(_) })
    }

    pub(crate) fn take_local_rq_baton(&mut self) -> Result<Option<RqSwitchBaton>, TaskError> {
        match &mut self.route {
            SwitchRoute::Local { rq_baton } => Ok(rq_baton.take()),
            SwitchRoute::Migration(_) => Err(TaskError::InvalidConfiguration),
        }
    }

    pub(crate) fn previous(&self) -> &ThreadCore {
        self.previous.as_ref()
    }

    pub(crate) fn retained_previous(&self) -> Option<&Arc<ThreadCore>> {
        self.previous.retained_arc()
    }

    pub(crate) const fn previous_requires_rq_baton(&self) -> bool {
        self.previous.requires_rq_baton()
    }

    pub(crate) fn incoming(&self) -> &ThreadCore {
        self.incoming.as_ref()
    }

    pub(crate) const fn incoming_ref(&self) -> SchedulerThreadRef {
        self.incoming
    }

    pub(crate) fn incoming_policy(&self) -> SchedulePolicy {
        self.incoming_policy.get()
    }

    pub(crate) fn migration_target(&self) -> Option<CpuId> {
        match &self.route {
            SwitchRoute::Local { .. } => None,
            SwitchRoute::Migration(migration) => Some(migration.target()),
        }
    }

    pub(crate) const fn previous_exited(&self) -> bool {
        matches!(self.previous_disposition, PreviousSwitchDisposition::Exited)
    }

    #[inline(always)]
    pub(crate) fn complete_migration(
        self,
        reclaim_ready: bool,
    ) -> Result<CompletedMigrationSwitchHandoff, TaskError> {
        let SwitchHandoff {
            previous,
            incoming,
            incoming_policy,
            previous_disposition,
            route,
        } = self;
        let SwitchRoute::Migration(migration) = route else {
            return Err(TaskError::InvalidConfiguration);
        };
        drop(previous);
        Ok(CompletedMigrationSwitchHandoff {
            incoming,
            incoming_policy,
            migration,
            reclaim_ready,
            previous_exited: matches!(previous_disposition, PreviousSwitchDisposition::Exited),
        })
    }
}
