//! Generation-checked thread park handshake.

use core::sync::atomic::{AtomicU8, Ordering};

use crate::{
    ScheduleDecision, ThreadId,
    timer::{TaskDeadlineRegistration, TaskDeadlineToken},
};

const WAIT_WAKE_QUEUED: u8 = 0;
const WAIT_WAKE_SELECTED: u8 = 1;
const WAIT_WAKE_DELIVERED: u8 = 2;
const WAIT_WAKE_CANCELLED: u8 = 3;
const WAIT_WAKE_INACTIVE: u8 = 4;

/// One queue notification claim state machine bound to an exact park attempt.
///
/// The containing wait queue owns entry order, while this atomic state owns
/// selection against concurrent cleanup. The scheduler owns delivery after all
/// fallible placement preparation, while timeout cleanup may close a selected
/// claim before that delivery point. An unavailable scheduler owner may return
/// a cancelled claim to the same wait entry for another selection attempt.
#[derive(Debug)]
pub(crate) struct WaitWakeClaim {
    thread: ThreadId,
    park_generation: u64,
    state: AtomicU8,
}

impl WaitWakeClaim {
    pub(crate) const fn new(thread: ThreadId, park_generation: u64) -> Self {
        Self {
            thread,
            park_generation,
            state: AtomicU8::new(WAIT_WAKE_QUEUED),
        }
    }

    pub(crate) const fn thread(&self) -> ThreadId {
        self.thread
    }

    pub(crate) const fn park_generation(&self) -> u64 {
        self.park_generation
    }

    pub(crate) fn state(&self) -> WaitWakeClaimState {
        match self.state.load(Ordering::Acquire) {
            WAIT_WAKE_QUEUED => WaitWakeClaimState::Queued,
            WAIT_WAKE_SELECTED => WaitWakeClaimState::Selected,
            WAIT_WAKE_DELIVERED => WaitWakeClaimState::Delivered,
            WAIT_WAKE_CANCELLED => WaitWakeClaimState::Cancelled,
            WAIT_WAKE_INACTIVE => WaitWakeClaimState::Inactive,
            _ => unreachable!("wait-wake claim state must be one of five closed states"),
        }
    }

    pub(crate) fn is_active(&self) -> bool {
        matches!(
            self.state(),
            WaitWakeClaimState::Queued
                | WaitWakeClaimState::Selected
                | WaitWakeClaimState::Cancelled
        )
    }

    pub(crate) fn select(&self) -> bool {
        self.state
            .compare_exchange(
                WAIT_WAKE_QUEUED,
                WAIT_WAKE_SELECTED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn deliver_selected(&self) -> bool {
        self.state
            .compare_exchange(
                WAIT_WAKE_SELECTED,
                WAIT_WAKE_DELIVERED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    pub(crate) fn cancel_selected(&self) -> bool {
        self.state
            .compare_exchange(
                WAIT_WAKE_SELECTED,
                WAIT_WAKE_CANCELLED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Returns a synchronously rejected delivery to its owning wait entry.
    ///
    /// The scheduler does not retain this claim after returning `Unavailable`.
    /// This transition races cleanup through the same atomic state: either the
    /// claim becomes queued again or cleanup closes it as inactive.
    pub(crate) fn requeue_cancelled(&self) -> bool {
        self.state
            .compare_exchange(
                WAIT_WAKE_CANCELLED,
                WAIT_WAKE_QUEUED,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    /// Closes this wait entry against later selection or unavailable requeue.
    ///
    /// Selection, scheduler delivery, and cleanup all transition this one
    /// atomic state. The scheduler's task lock still owns runnable placement;
    /// no separate wait-entry lock is needed to serialize these terminal CAS
    /// operations.
    pub(crate) fn deactivate(&self) -> bool {
        loop {
            let current = self.state.load(Ordering::Acquire);
            match current {
                WAIT_WAKE_DELIVERED => return true,
                WAIT_WAKE_INACTIVE => return false,
                WAIT_WAKE_QUEUED | WAIT_WAKE_SELECTED | WAIT_WAKE_CANCELLED => {
                    if self
                        .state
                        .compare_exchange(
                            current,
                            WAIT_WAKE_INACTIVE,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_ok()
                    {
                        return false;
                    }
                }
                _ => unreachable!("wait-wake claim state must be one of five closed states"),
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WaitWakeClaimState {
    Queued,
    Selected,
    Delivered,
    Cancelled,
    Inactive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum WaitWakeDelivery {
    Delivered,
    Cancelled,
    Exited,
    Unavailable,
}

/// Move-only ownership of one park attempt and its optional timeout deadline.
///
/// ```compile_fail
/// # use ax_task::ParkTicket;
/// fn duplicate(ticket: ParkTicket) {
///     let first = ticket;
///     let second = ticket;
///     drop((first, second));
/// }
/// ```
#[must_use = "a prepared park and its deadline must be committed or cancelled"]
#[derive(Debug, Eq, PartialEq)]
pub struct ParkTicket {
    thread: ThreadId,
    generation: u64,
    deadline: Option<TaskDeadlineRegistration>,
    resolved: bool,
}

impl ParkTicket {
    pub(crate) const fn new(thread: ThreadId, generation: u64) -> Self {
        Self {
            thread,
            generation,
            deadline: None,
            resolved: false,
        }
    }

    /// Returns the thread that prepared this park attempt.
    pub const fn thread(&self) -> ThreadId {
        self.thread
    }

    /// Returns the monotonically increasing attempt generation.
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    pub(crate) fn attach_deadline(
        &mut self,
        deadline: TaskDeadlineRegistration,
    ) -> Result<(), TaskDeadlineRegistration> {
        if self.deadline.is_some() {
            Err(deadline)
        } else {
            self.deadline = Some(deadline);
            Ok(())
        }
    }

    pub(crate) const fn deadline(&self) -> Option<&TaskDeadlineRegistration> {
        self.deadline.as_ref()
    }

    pub(crate) fn clear_deadline(&mut self, deadline: TaskDeadlineToken) -> bool {
        if self
            .deadline
            .as_ref()
            .is_some_and(|registration| registration.token() == deadline)
        {
            let _registration = self.deadline.take();
            true
        } else {
            false
        }
    }

    pub(crate) fn mark_resolved(&mut self) {
        self.resolved = true;
    }

    pub(crate) const fn is_resolved(&self) -> bool {
        self.resolved
    }

    pub(crate) const fn has_deadline(&self) -> bool {
        self.deadline.is_some()
    }
}

/// Result of publishing the `PARKING` phase.
#[derive(Debug, Eq, PartialEq)]
pub enum ParkPrepare {
    /// A preceding notification was consumed, so the caller must not block.
    Notified,
    /// The caller published `PARKING` and may proceed to the commit phase.
    Prepared(ParkTicket),
}

/// Result of rechecking a prepared park at the scheduler safe point.
#[derive(Clone, Copy, Debug)]
pub enum ParkCommit {
    /// A concurrent notification cancelled the park before schedule-out.
    Notified,
    /// The thread committed `BLOCKED` and selected its replacement.
    Blocked(ScheduleDecision),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_wait_wake_claim_can_retry_the_same_park() {
        let claim = WaitWakeClaim::new(ThreadId::from_parts(7, 3), 11);

        assert!(claim.select());
        assert!(claim.cancel_selected());
        assert!(claim.requeue_cancelled());
        assert!(claim.select());
        assert!(claim.deliver_selected());
        assert_eq!(claim.state(), WaitWakeClaimState::Delivered);
    }

    #[test]
    fn delivered_wait_wake_claim_cannot_be_requeued() {
        let claim = WaitWakeClaim::new(ThreadId::from_parts(7, 3), 11);

        assert!(claim.select());
        assert!(claim.deliver_selected());
        assert!(!claim.requeue_cancelled());
        assert_eq!(claim.state(), WaitWakeClaimState::Delivered);
    }

    #[test]
    fn deactivated_queued_claim_cannot_be_selected_or_requeued() {
        let claim = WaitWakeClaim::new(ThreadId::from_parts(7, 3), 11);

        assert!(!claim.deactivate());
        assert!(!claim.select());
        assert!(!claim.requeue_cancelled());
        assert!(!claim.is_active());
        assert_eq!(claim.state(), WaitWakeClaimState::Inactive);
    }

    #[test]
    fn deactivation_cancels_a_selected_claim_before_delivery() {
        let claim = WaitWakeClaim::new(ThreadId::from_parts(7, 3), 11);

        assert!(claim.select());
        assert!(!claim.deactivate());
        assert!(!claim.deliver_selected());
        assert_eq!(claim.state(), WaitWakeClaimState::Inactive);
    }

    #[test]
    fn deactivation_observes_a_delivered_claim_idempotently() {
        let claim = WaitWakeClaim::new(ThreadId::from_parts(7, 3), 11);

        assert!(claim.select());
        assert!(claim.deliver_selected());
        assert!(claim.deactivate());
        assert!(claim.deactivate());
        assert_eq!(claim.state(), WaitWakeClaimState::Delivered);
    }

    #[test]
    fn deactivated_cancelled_claim_cannot_be_requeued() {
        let claim = WaitWakeClaim::new(ThreadId::from_parts(7, 3), 11);

        assert!(claim.select());
        assert!(claim.cancel_selected());
        assert!(!claim.deactivate());
        assert!(!claim.requeue_cancelled());
        assert_eq!(claim.state(), WaitWakeClaimState::Inactive);
    }
}
