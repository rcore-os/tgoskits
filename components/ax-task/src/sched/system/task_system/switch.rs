//! Owner selection, schedule-out, and switch-handoff construction.

use super::*;
use crate::sched::{
    algorithm::{LinkedRqTaskRef, PickTaskResult, RtEligibility},
    system::cpu::{PreviousSwitchDisposition, PreviousSwitchOwnership, SwitchHandoff},
};

pub(super) struct OwnerScheduleOut {
    pub(super) migration: Option<PreparedMigrationDelivery>,
}

pub(super) enum OwnerRqScheduleOut {
    Idle { thread: ThreadId },
    LinkedRealtime { previous: LinkedRqTaskRef },
    Unlinked { thread: ThreadId },
}

impl OwnerRqScheduleOut {
    pub(super) const fn is_linked_realtime(&self) -> bool {
        matches!(self, Self::LinkedRealtime { .. })
    }
}

pub(super) struct OwnerRqScheduledOut {
    pub(super) core: PreviousSwitchOwnership,
    pub(super) endpoint: SwitchEndpoint,
    pub(super) fifo: bool,
    pub(super) urgency: SchedulingUrgency,
    pub(super) realtime_yield_head: Option<LinkedRqTaskRef>,
}

mod handoff;

mod schedule_out;

mod publication;

mod placement;

mod selection;
