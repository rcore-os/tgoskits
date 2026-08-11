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
    RuntimeTailFinished,
}

pub(crate) struct CompletedSwitchHandoff {
    pub(crate) previous: Arc<ThreadCore>,
    pub(crate) incoming: Arc<ThreadCore>,
    pub(crate) migration: Option<PreparedMigrationDelivery>,
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
        matches!(self.phase, SwitchHandoffPhase::RuntimeTailFinished)
    }

    pub(crate) fn finish_runtime_tail(mut self) -> Result<Self, TaskError> {
        if self.phase != SwitchHandoffPhase::Prepared {
            return Err(TaskError::InvalidConfiguration);
        }
        self.phase = SwitchHandoffPhase::RuntimeTailFinished;
        Ok(self)
    }

    pub(crate) fn into_runtime_finished(self) -> Result<CompletedSwitchHandoff, TaskError> {
        if self.phase != SwitchHandoffPhase::RuntimeTailFinished {
            return Err(TaskError::InvalidConfiguration);
        }
        Ok(CompletedSwitchHandoff {
            previous: self.previous,
            incoming: self.incoming,
            migration: self.migration,
        })
    }
}
