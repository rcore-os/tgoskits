//! Move-only context-switch tail ownership.

use super::super::*;

/// State committed before an architecture switch and consumed by switch tail.
#[derive(Debug)]
pub(crate) struct SwitchHandoff {
    phase: SwitchHandoffPhase,
    previous: Arc<ThreadCore>,
    incoming: Arc<ThreadCore>,
    switch_timestamp_ns: u64,
    migration: Option<PreparedMigrationDelivery>,
    rq_baton: Option<RqSwitchBaton>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SwitchHandoffPhase {
    Prepared,
    RuntimeTailFinished { reclaim_ready: bool },
}

pub(crate) struct CompletedSwitchHandoff {
    pub(crate) incoming: Arc<ThreadCore>,
    pub(crate) switch_timestamp_ns: u64,
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
            switch_timestamp_ns: 0,
            migration,
            rq_baton: None,
        }
    }

    pub(crate) fn install_rq_baton(&mut self, baton: RqSwitchBaton) -> Result<(), TaskError> {
        if self.phase != SwitchHandoffPhase::Prepared
            || self.migration.is_some()
            || self.rq_baton.is_some()
        {
            return Err(TaskError::InvalidConfiguration);
        }
        self.rq_baton = Some(baton);
        Ok(())
    }

    pub(crate) const fn has_rq_baton(&self) -> bool {
        self.rq_baton.is_some()
    }

    pub(crate) fn take_rq_baton(&mut self) -> Option<RqSwitchBaton> {
        self.rq_baton.take()
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

    pub(crate) fn finish_runtime_tail(
        mut self,
        reclaim_ready: bool,
        switch_timestamp_ns: u64,
    ) -> Result<Self, TaskError> {
        if self.phase != SwitchHandoffPhase::Prepared {
            return Err(TaskError::InvalidConfiguration);
        }
        self.switch_timestamp_ns = switch_timestamp_ns;
        self.phase = SwitchHandoffPhase::RuntimeTailFinished { reclaim_ready };
        Ok(self)
    }

    pub(crate) fn into_runtime_finished(self) -> Result<CompletedSwitchHandoff, TaskError> {
        let SwitchHandoff {
            previous,
            incoming,
            switch_timestamp_ns,
            migration,
            rq_baton,
            phase,
        } = self;
        if rq_baton.is_some() {
            return Err(TaskError::InvalidConfiguration);
        }
        let SwitchHandoffPhase::RuntimeTailFinished { reclaim_ready } = phase else {
            return Err(TaskError::InvalidConfiguration);
        };
        drop(previous);
        Ok(CompletedSwitchHandoff {
            incoming,
            switch_timestamp_ns,
            migration,
            reclaim_ready,
        })
    }
}
