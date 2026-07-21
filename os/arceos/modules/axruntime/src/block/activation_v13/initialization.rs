//! Bounded controller initialization on its final maintenance owner.

use alloc::boxed::Box;

use ax_driver::block::RdifBlockPreparedOwner;
use rdif_block::{
    ControlProgress, ControlSchedule, ControlTrigger, ControllerFault, ControllerPublicationReady,
    IrqServiceDecision, PendingBlockIrq,
};

use super::{
    BoundEvidenceSource, PendingRetentionFailure, PendingSourceFault, SourceServiceFailure,
    V13MaintenanceEvent, source::SourceDrainProgress,
};
use crate::maintenance::{MaintenanceSession, MaintenanceWaitOutcome};

const INIT_TRANSITION_BUDGET: usize = 64;

pub(super) fn drive_controller_initialization(
    prepared: &mut RdifBlockPreparedOwner,
    session: &MaintenanceSession<V13MaintenanceEvent>,
    sources: &mut [BoundEvidenceSource],
) -> Result<ControllerPublicationReady, InitDriveFailure> {
    let mut trigger = ControlTrigger::Start {
        now_ns: monotonic_now(),
    };
    let mut immediate_budget = ImmediateProgressBudget::new(INIT_TRANSITION_BUDGET);
    loop {
        let poll = prepared
            .prepared_mut()
            .service_control(trigger)
            .map_err(|failure| InitDriveFailure::Control {
                _failure: Box::new(failure),
            })?;
        let (progress, evidence) = poll.into_parts();
        apply_evidence_decision(prepared, sources, evidence)?;
        match progress {
            ControlProgress::PublicationReady(ready) => {
                if let Some(source) = sources.iter().find(|source| source.has_pending_owner()) {
                    return Err(InitDriveFailure::PendingAtPublication {
                        _source: source.source(),
                    });
                }
                return Ok(ready);
            }
            ControlProgress::Pending(schedule) => {
                trigger = wait_for_next_trigger(session, sources, schedule, &mut immediate_budget)?;
            }
            ControlProgress::Failed(error) => {
                return Err(InitDriveFailure::Initialization { _error: error });
            }
            unexpected @ (ControlProgress::DmaQuiesced(_) | ControlProgress::Reinitialized(_)) => {
                return Err(InitDriveFailure::Unexpected {
                    _progress: Box::new(unexpected),
                });
            }
        }
    }
}

fn apply_evidence_decision(
    prepared: &mut RdifBlockPreparedOwner,
    sources: &mut [BoundEvidenceSource],
    decision: Option<IrqServiceDecision>,
) -> Result<(), InitDriveFailure> {
    let Some(decision) = decision else {
        return Ok(());
    };
    let source_id = decision.evidence_id().source();
    let Some(source) = sources
        .iter_mut()
        .find(|source| source.source() == source_id)
    else {
        return Err(InitDriveFailure::UnknownSource {
            _decision: Box::new(decision),
        });
    };
    match decision {
        IrqServiceDecision::Retained(pending) => {
            source
                .retain_pending(pending)
                .map_err(|failure| InitDriveFailure::Retention {
                    _failure: Box::new(failure),
                })
        }
        IrqServiceDecision::Drained(drained) => {
            let progress = source
                .complete_drained_with(drained, |evidence| {
                    prepared
                        .prepared_mut()
                        .commit_drained_evidence(evidence)
                })
                .map_err(|failure| InitDriveFailure::SourceService {
                    _failure: Box::new(failure),
                })?;
            if let SourceDrainProgress::Redelivered(pending) = progress {
                source.retain_pending(pending).map_err(|failure| {
                    InitDriveFailure::Retention {
                        _failure: Box::new(failure),
                    }
                })?;
            }
            Ok(())
        }
        IrqServiceDecision::Recover { evidence, fault } => Err(InitDriveFailure::Recovery {
            _evidence: Box::new(evidence),
            _fault: fault,
        }),
    }
}

fn wait_for_next_trigger(
    session: &MaintenanceSession<V13MaintenanceEvent>,
    sources: &mut [BoundEvidenceSource],
    schedule: ControlSchedule,
    immediate_budget: &mut ImmediateProgressBudget,
) -> Result<ControlTrigger, InitDriveFailure> {
    loop {
        if let Some(fault) = sources.iter().find_map(BoundEvidenceSource::take_fault) {
            return Err(InitDriveFailure::SourceFault {
                _fault: Box::new(fault),
            });
        }
        if let Some(pending) = sources
            .iter_mut()
            .find_map(BoundEvidenceSource::take_pending)
        {
            account_immediate_progress(immediate_budget)?;
            return Ok(ControlTrigger::Irq {
                now_ns: monotonic_now(),
                evidence: pending,
            });
        }

        // Mailbox entries are Copy-only activation hints. The move-only owner
        // remains in the exact source slot, so clearing a bounded batch before
        // rescanning cannot lose evidence or confuse sparse source IDs.
        let drain = session
            .drain_owner(INIT_TRANSITION_BUDGET, |_| {})
            .map_err(|error| InitDriveFailure::Maintenance { _error: error })?;
        if let Some(fault) = sources.iter().find_map(BoundEvidenceSource::take_fault) {
            return Err(InitDriveFailure::SourceFault {
                _fault: Box::new(fault),
            });
        }
        if let Some(pending) = sources
            .iter_mut()
            .find_map(BoundEvidenceSource::take_pending)
        {
            account_immediate_progress(immediate_budget)?;
            return Ok(ControlTrigger::Irq {
                now_ns: monotonic_now(),
                evidence: pending,
            });
        }
        if schedule.internal_progress_ready() {
            account_immediate_progress(immediate_budget)?;
            return Ok(ControlTrigger::InternalProgress {
                now_ns: monotonic_now(),
            });
        }
        if drain.pending() {
            account_immediate_progress(immediate_budget)?;
            continue;
        }

        let outcome = match schedule.wake_at_ns() {
            Some(deadline) => session
                .wait_for_pending_until(deadline)
                .map_err(|error| InitDriveFailure::Maintenance { _error: error })?,
            None => {
                session
                    .wait_for_pending()
                    .map_err(|error| InitDriveFailure::Maintenance { _error: error })?;
                MaintenanceWaitOutcome::ConditionMet
            }
        };
        if matches!(outcome, MaintenanceWaitOutcome::TimedOut) {
            immediate_budget.reset();
            return Ok(ControlTrigger::ProtocolDeadline {
                now_ns: monotonic_now(),
            });
        }
        immediate_budget.reset();
    }
}

fn account_immediate_progress(
    budget: &mut ImmediateProgressBudget,
) -> Result<(), InitDriveFailure> {
    if budget.consume() {
        crate::task::yield_current_cpu()
            .map_err(|error| InitDriveFailure::Task { _error: error })?;
    }
    Ok(())
}

struct ImmediateProgressBudget {
    used: usize,
    limit: usize,
}

impl ImmediateProgressBudget {
    const fn new(limit: usize) -> Self {
        Self { used: 0, limit }
    }

    fn consume(&mut self) -> bool {
        self.used += 1;
        if self.used < self.limit {
            return false;
        }
        self.used = 0;
        true
    }

    fn reset(&mut self) {
        self.used = 0;
    }
}

fn monotonic_now() -> u64 {
    ax_hal::time::monotonic_time_nanos()
}

/// Initialization failure retaining every move-only protocol owner it names.
pub(super) enum InitDriveFailure {
    Control {
        _failure: Box<rdif_block::ControlServiceFailure>,
    },
    SourceFault {
        _fault: Box<PendingSourceFault>,
    },
    SourceService {
        _failure: Box<SourceServiceFailure>,
    },
    Retention {
        _failure: Box<PendingRetentionFailure>,
    },
    Recovery {
        _evidence: Box<PendingBlockIrq>,
        _fault: ControllerFault,
    },
    UnknownSource {
        _decision: Box<IrqServiceDecision>,
    },
    PendingAtPublication {
        _source: rdif_block::IrqSourceId,
    },
    Initialization {
        _error: rdif_block::InitError,
    },
    Unexpected {
        _progress: Box<ControlProgress>,
    },
    Maintenance {
        _error: crate::maintenance::MaintenanceError,
    },
    Task {
        _error: crate::task::TaskError,
    },
}

impl InitDriveFailure {
    pub(super) const fn phase(&self) -> &'static str {
        match self {
            Self::Control { .. } => "control contract",
            Self::SourceFault { .. } => "IRQ source fault",
            Self::SourceService { .. } => "IRQ evidence completion",
            Self::Retention { .. } => "IRQ evidence retention",
            Self::Recovery { .. } => "controller recovery requested during init",
            Self::UnknownSource { .. } => "unknown IRQ source",
            Self::PendingAtPublication { .. } => "publication with pending IRQ evidence",
            Self::Initialization { .. } => "controller initialization",
            Self::Unexpected { .. } => "unexpected controller lifecycle proof",
            Self::Maintenance { .. } => "maintenance wait",
            Self::Task { .. } => "maintenance yield",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_evidence_cannot_spin_past_the_shared_transition_budget() {
        let mut budget = ImmediateProgressBudget::new(64);
        let yields = (0..129).filter(|_| budget.consume()).count();

        assert_eq!(yields, 2);
        assert_eq!(budget.used, 1);
    }

    #[test]
    fn a_real_park_resets_the_immediate_transition_budget() {
        let mut budget = ImmediateProgressBudget::new(4);
        assert!(!budget.consume());
        assert!(!budget.consume());

        budget.reset();

        assert!(!budget.consume());
        assert_eq!(budget.used, 1);
    }
}
