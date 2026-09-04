//! Move-only context-switch tail ownership.

use super::super::*;

/// State committed before an architecture switch and consumed by switch tail.
#[derive(Debug)]
pub(crate) struct SwitchHandoff {
    previous: Arc<ThreadCore>,
    incoming: SchedulerThreadRef,
    incoming_policy: SchedulePolicy,
    previous_disposition: PreviousSwitchDisposition,
    route: SwitchRoute,
    rq_state: SwitchRqState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PreviousSwitchDisposition {
    Live,
    Exited,
}

#[derive(Debug)]
enum SwitchRoute {
    Local,
    Migration(PreparedMigrationDelivery),
}

#[derive(Debug)]
enum SwitchRqState {
    Released,
    Retained(RqSwitchBaton),
}

pub(crate) struct CompletedSwitchHandoff {
    pub(crate) incoming: SchedulerThreadRef,
    pub(crate) incoming_policy: SchedulePolicy,
    pub(crate) migration: Option<PreparedMigrationDelivery>,
    pub(crate) reclaim_ready: bool,
    pub(crate) previous_exited: bool,
}

impl SwitchHandoff {
    pub(crate) fn prepared(
        previous: Arc<ThreadCore>,
        incoming: SchedulerThreadRef,
        incoming_policy: SchedulePolicy,
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
                None => SwitchRoute::Local,
            },
            rq_state: SwitchRqState::Released,
        }
    }

    pub(crate) fn install_rq_baton(&mut self, baton: RqSwitchBaton) -> Result<(), TaskError> {
        if !matches!(self.route, SwitchRoute::Local)
            || !matches!(self.rq_state, SwitchRqState::Released)
        {
            return Err(TaskError::InvalidConfiguration);
        }
        self.rq_state = SwitchRqState::Retained(baton);
        Ok(())
    }

    pub(crate) fn has_rq_baton(&self) -> bool {
        matches!(self.rq_state, SwitchRqState::Retained(_))
    }

    pub(crate) fn take_rq_baton(&mut self) -> Option<RqSwitchBaton> {
        let rq_state = core::mem::replace(&mut self.rq_state, SwitchRqState::Released);
        match rq_state {
            SwitchRqState::Released => None,
            SwitchRqState::Retained(baton) => Some(baton),
        }
    }

    pub(crate) fn previous(&self) -> &Arc<ThreadCore> {
        &self.previous
    }

    pub(crate) fn incoming(&self) -> &ThreadCore {
        self.incoming.as_ref()
    }

    pub(crate) fn migration_target(&self) -> Option<CpuId> {
        match &self.route {
            SwitchRoute::Local => None,
            SwitchRoute::Migration(migration) => Some(migration.target()),
        }
    }

    pub(crate) const fn previous_exited(&self) -> bool {
        matches!(self.previous_disposition, PreviousSwitchDisposition::Exited)
    }

    #[inline(always)]
    pub(crate) fn complete(self, reclaim_ready: bool) -> Result<CompletedSwitchHandoff, TaskError> {
        let SwitchHandoff {
            previous,
            incoming,
            incoming_policy,
            previous_disposition,
            route,
            rq_state,
        } = self;
        if !matches!(rq_state, SwitchRqState::Released) {
            return Err(TaskError::InvalidConfiguration);
        }
        drop(previous);
        Ok(CompletedSwitchHandoff {
            incoming,
            incoming_policy,
            migration: match route {
                SwitchRoute::Local => None,
                SwitchRoute::Migration(migration) => Some(migration),
            },
            reclaim_ready,
            previous_exited: matches!(previous_disposition, PreviousSwitchDisposition::Exited),
        })
    }
}
