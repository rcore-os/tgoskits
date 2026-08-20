//! Move-only context-switch tail ownership.

use super::super::*;

/// State committed before an architecture switch and consumed by switch tail.
#[derive(Debug)]
pub(crate) struct SwitchHandoff {
    phase: SwitchHandoffPhase,
    previous: Arc<ThreadCore>,
    incoming: Arc<ThreadCore>,
    migration: Option<PreparedMigrationDelivery>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SwitchHandoffPhase {
    Prepared,
    RuntimeTailFinished { reclaim_ready: bool },
}

pub(crate) struct CompletedSwitchHandoff {
    pub(crate) previous: Arc<ThreadCore>,
    pub(crate) incoming: Arc<ThreadCore>,
    pub(crate) migration: Option<PreparedMigrationDelivery>,
    pub(crate) reclaim_ready: bool,
}

impl SwitchHandoff {
    pub(crate) fn prepared(
        previous: Arc<ThreadCore>,
        incoming: Arc<ThreadCore>,
        migration: Option<PreparedMigrationDelivery>,
    ) -> Self {
        Self {
            phase: SwitchHandoffPhase::Prepared,
            previous,
            incoming,
            migration,
        }
    }

    pub(crate) fn previous(&self) -> &Arc<ThreadCore> {
        &self.previous
    }

    pub(crate) fn incoming(&self) -> &Arc<ThreadCore> {
        &self.incoming
    }

    pub(crate) fn migration_target(&self) -> Option<CpuId> {
        self.migration
            .as_ref()
            .map(PreparedMigrationDelivery::target)
    }

    pub(crate) const fn runtime_tail_is_finished(&self) -> bool {
        matches!(self.phase, SwitchHandoffPhase::RuntimeTailFinished { .. })
    }

    pub(crate) fn finish_runtime_tail(mut self, reclaim_ready: bool) -> Result<Self, TaskError> {
        if self.phase != SwitchHandoffPhase::Prepared {
            return Err(TaskError::InvalidConfiguration);
        }
        self.phase = SwitchHandoffPhase::RuntimeTailFinished { reclaim_ready };
        Ok(self)
    }

    pub(crate) fn into_runtime_finished(self) -> Result<CompletedSwitchHandoff, TaskError> {
        let SwitchHandoffPhase::RuntimeTailFinished { reclaim_ready } = self.phase else {
            return Err(TaskError::InvalidConfiguration);
        };
        Ok(CompletedSwitchHandoff {
            previous: self.previous,
            incoming: self.incoming,
            migration: self.migration,
            reclaim_ready,
        })
    }
}
