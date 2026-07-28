//! Generation-bearing CPU ownership state for task-bound PMU events.

use super::target::PerfCpuId;

/// Identity returned by the per-CPU sampling registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SampleRegistration {
    owner: PerfCpuId,
    counter: usize,
    generation: u64,
}

impl SampleRegistration {
    /// Creates a registry identity after one slot has been published.
    pub(crate) const fn new(owner: PerfCpuId, counter: usize, generation: u64) -> Self {
        Self {
            owner,
            counter,
            generation,
        }
    }

    /// Returns the CPU whose registry owns the slot.
    pub(crate) const fn owner(self) -> PerfCpuId {
        self.owner
    }

    /// Returns the programmable PMU counter index.
    pub(crate) const fn counter(self) -> usize {
        self.counter
    }

    /// Returns the globally unique slot generation.
    pub(crate) const fn generation(self) -> u64 {
        self.generation
    }
}

/// One schedule-in attempt, before the hardware slot is fully running.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PmuArmTicket {
    owner: PerfCpuId,
    generation: u64,
}

/// One hardware-running schedule generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PmuRunLease {
    owner: PerfCpuId,
    generation: u64,
    registration: Option<SampleRegistration>,
}

impl PmuRunLease {
    /// Returns the CPU that owns the programmed counter.
    pub(crate) const fn owner(self) -> PerfCpuId {
        self.owner
    }

    /// Returns the sampling slot identity, when this is a sampling event.
    pub(crate) const fn registration(self) -> Option<SampleRegistration> {
        self.registration
    }

    const fn generation(self) -> u64 {
        self.generation
    }
}

/// Action required after an fd/task teardown or disable request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PmuCloseAction {
    /// A previous fd/task teardown already released this event.
    AlreadyClosed,
    /// No hardware generation remains reachable.
    Complete,
    /// The owner CPU must stop this exact generation.
    Stop(PmuRunLease),
}

/// Result of attempting to claim an exact owner-CPU stop.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PmuStopClaim {
    /// This caller exclusively owns the hardware stop transaction.
    Claimed(PmuRunLease),
    /// The same generation was already stopped by switch-out.
    AlreadyComplete,
    /// Another owner-CPU path is currently stopping this generation.
    InProgress,
    /// The requested generation is not the active or last-completed one.
    Stale,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PmuStopGoal {
    Detach,
    Close,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PmuRunPhase {
    Detached,
    Arming(PmuArmTicket),
    Registered(PmuArmTicket, SampleRegistration),
    Running(PmuRunLease),
    StopRequested(PmuRunLease, PmuStopGoal),
    Stopping(PmuRunLease, PmuStopGoal),
    Closed,
}

/// Serialized lifecycle for scheduler hooks and task-context control.
#[derive(Debug)]
pub(crate) struct PmuRunState {
    phase: PmuRunPhase,
    next_generation: u64,
    last_stopped_generation: u64,
}

impl PmuRunState {
    /// Creates a detached event.
    pub(crate) const fn new() -> Self {
        Self {
            phase: PmuRunPhase::Detached,
            next_generation: 0,
            last_stopped_generation: 0,
        }
    }

    /// Starts one schedule-in generation.
    pub(crate) fn begin_arm(&mut self, owner: PerfCpuId) -> Option<PmuArmTicket> {
        if self.phase != PmuRunPhase::Detached {
            return None;
        }
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("PMU run generation exhausted");
        let ticket = PmuArmTicket {
            owner,
            generation: self.next_generation,
        };
        self.phase = PmuRunPhase::Arming(ticket);
        Some(ticket)
    }

    /// Publishes the exact per-CPU registry identity for this arm attempt.
    pub(crate) fn publish_registration(
        &mut self,
        ticket: PmuArmTicket,
        registration: SampleRegistration,
    ) {
        assert_eq!(self.phase, PmuRunPhase::Arming(ticket));
        self.phase = PmuRunPhase::Registered(ticket, registration);
    }

    /// Marks the hardware counter running after the registry is reachable.
    pub(crate) fn finish_arm(&mut self, ticket: PmuArmTicket) {
        let (observed, registration) = match self.phase {
            PmuRunPhase::Arming(observed) => (observed, None),
            PmuRunPhase::Registered(observed, registration) => (observed, Some(registration)),
            _ => panic!("PMU arm completed from an invalid lifecycle phase"),
        };
        assert_eq!(observed, ticket);
        self.phase = PmuRunPhase::Running(PmuRunLease {
            owner: ticket.owner,
            generation: ticket.generation,
            registration,
        });
    }

    /// Aborts an arm attempt before any registry entry is published.
    pub(crate) fn cancel_arm(&mut self, ticket: PmuArmTicket) {
        assert_eq!(self.phase, PmuRunPhase::Arming(ticket));
        self.phase = PmuRunPhase::Detached;
    }

    /// Returns the hardware-live generation, including a requested/in-flight stop.
    ///
    /// A close request must remain visible here so the task's switch-out path
    /// can quiesce PMU hardware before an affine worker is scheduled.
    pub(crate) const fn running(&self) -> Option<PmuRunLease> {
        match self.phase {
            PmuRunPhase::Running(lease)
            | PmuRunPhase::StopRequested(lease, _)
            | PmuRunPhase::Stopping(lease, _) => Some(lease),
            _ => None,
        }
    }

    /// Claims the hardware generation for the scheduler switch-out path.
    pub(crate) fn claim_schedule_out(&mut self) -> Option<PmuRunLease> {
        let (lease, goal) = match self.phase {
            PmuRunPhase::Running(lease) => (lease, PmuStopGoal::Detach),
            PmuRunPhase::StopRequested(lease, goal) => (lease, goal),
            _ => return None,
        };
        self.phase = PmuRunPhase::Stopping(lease, goal);
        Some(lease)
    }

    /// Claims a stop previously requested by disable or close.
    pub(crate) fn claim_requested_stop(&mut self, lease: PmuRunLease) -> PmuStopClaim {
        match self.phase {
            PmuRunPhase::StopRequested(observed, goal) if observed == lease => {
                self.phase = PmuRunPhase::Stopping(observed, goal);
                PmuStopClaim::Claimed(observed)
            }
            PmuRunPhase::Stopping(observed, _) if observed == lease => PmuStopClaim::InProgress,
            PmuRunPhase::Detached | PmuRunPhase::Closed
                if self.last_stopped_generation == lease.generation() =>
            {
                PmuStopClaim::AlreadyComplete
            }
            _ => PmuStopClaim::Stale,
        }
    }

    /// Publishes the result of one exact owner-CPU stop.
    pub(crate) fn finish_owner_stop(&mut self, lease: PmuRunLease) {
        let goal = match self.phase {
            PmuRunPhase::Stopping(observed, goal) if observed == lease => goal,
            _ => panic!("PMU stop completed from an invalid lifecycle phase"),
        };
        self.last_stopped_generation = lease.generation();
        self.phase = match goal {
            PmuStopGoal::Detach => PmuRunPhase::Detached,
            PmuStopGoal::Close => PmuRunPhase::Closed,
        };
    }

    /// Returns a failed owner-CPU stop to the requested state for retry.
    ///
    /// The exact generation and the strongest requested goal are retained. In
    /// particular, a concurrent close that upgraded a disable transaction must
    /// remain a permanent-close request after the architecture operation fails.
    pub(crate) fn abort_owner_stop(&mut self, lease: PmuRunLease) {
        let goal = match self.phase {
            PmuRunPhase::Stopping(observed, goal) if observed == lease => goal,
            _ => panic!("PMU stop aborted from an invalid lifecycle phase"),
        };
        self.phase = PmuRunPhase::StopRequested(lease, goal);
    }

    /// Reports whether permanent teardown was requested or completed.
    pub(crate) const fn is_stopping(&self) -> bool {
        matches!(
            self.phase,
            PmuRunPhase::StopRequested(_, PmuStopGoal::Close)
                | PmuRunPhase::Stopping(_, PmuStopGoal::Close)
                | PmuRunPhase::Closed
        )
    }

    /// Requests an owner-CPU stop without permanently closing the event.
    pub(crate) fn begin_disable(&mut self) -> PmuCloseAction {
        self.request_stop(PmuStopGoal::Detach)
    }

    /// Starts idempotent permanent teardown.
    pub(crate) fn begin_close(&mut self) -> PmuCloseAction {
        self.request_stop(PmuStopGoal::Close)
    }

    fn request_stop(&mut self, requested_goal: PmuStopGoal) -> PmuCloseAction {
        let (lease, phase_is_stopping, current_goal) = match self.phase {
            PmuRunPhase::Registered(ticket, registration) => (
                PmuRunLease {
                    owner: ticket.owner,
                    generation: ticket.generation,
                    registration: Some(registration),
                },
                false,
                requested_goal,
            ),
            PmuRunPhase::Running(lease) => (lease, false, requested_goal),
            PmuRunPhase::StopRequested(lease, goal) => (lease, false, goal),
            PmuRunPhase::Stopping(lease, goal) => (lease, true, goal),
            PmuRunPhase::Detached => {
                if requested_goal == PmuStopGoal::Close {
                    self.phase = PmuRunPhase::Closed;
                }
                return PmuCloseAction::Complete;
            }
            PmuRunPhase::Closed => return PmuCloseAction::AlreadyClosed,
            PmuRunPhase::Arming(_) => {
                panic!("PMU stop observed an arm before registry publication")
            }
        };
        let goal = if requested_goal == PmuStopGoal::Close {
            PmuStopGoal::Close
        } else {
            current_goal
        };
        self.phase = if phase_is_stopping {
            PmuRunPhase::Stopping(lease, goal)
        } else {
            PmuRunPhase::StopRequested(lease, goal)
        };
        PmuCloseAction::Stop(lease)
    }
}
