//! Generation-bearing CPU ownership state for task-bound PMU events.

use super::cpu_id::PerfCpuId;

/// Hardware counter selected for one PMU event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Counter {
    Cycle,
    Programmable(usize),
}

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
    counter: Counter,
    generation: u64,
}

/// One hardware-running schedule generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PmuRunLease {
    owner: PerfCpuId,
    counter: Counter,
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

    pub(super) const fn counter(self) -> Counter {
        self.counter
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
    pub(crate) fn begin_arm(&mut self, owner: PerfCpuId, counter: Counter) -> Option<PmuArmTicket> {
        if self.phase != PmuRunPhase::Detached {
            return None;
        }
        self.next_generation = self
            .next_generation
            .checked_add(1)
            .expect("PMU run generation exhausted");
        let ticket = PmuArmTicket {
            owner,
            counter,
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
            counter: ticket.counter,
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
                    counter: ticket.counter,
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

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    const TEST_COUNTER: Counter = Counter::Programmable(2);

    #[test]
    fn cancelled_arm_returns_to_the_detached_state() {
        let cpu = PerfCpuId::new(0);
        let mut state = PmuRunState::new();
        let arm = state.begin_arm(cpu, Counter::Cycle).unwrap();

        state.cancel_arm(arm);

        assert!(state.begin_arm(cpu, Counter::Cycle).is_some());
    }

    #[test]
    fn close_after_registry_publish_must_disarm_before_reclaim() {
        let cpu = PerfCpuId::new(1);
        let mut state = PmuRunState::new();
        let arm = state.begin_arm(cpu, TEST_COUNTER).unwrap();
        let registration = SampleRegistration::new(cpu, 3, 17);
        state.publish_registration(arm, registration);

        let PmuCloseAction::Stop(lease) = state.begin_close() else {
            panic!("a slot is IRQ-reachable before the legacy running flag is published");
        };
        assert_eq!(lease.owner(), cpu);
        assert_eq!(lease.counter(), TEST_COUNTER);
        assert_eq!(lease.registration(), Some(registration));
    }

    #[test]
    fn fully_running_generation_is_disarmed_on_its_owner_cpu() {
        let cpu = PerfCpuId::new(2);
        let mut state = PmuRunState::new();
        let arm = state.begin_arm(cpu, TEST_COUNTER).unwrap();
        let registration = SampleRegistration::new(cpu, 4, 23);
        state.publish_registration(arm, registration);
        state.finish_arm(arm);

        let PmuCloseAction::Stop(lease) = state.begin_close() else {
            panic!("running registration was not disarmed");
        };
        let observed = lease.registration().unwrap();
        assert_eq!(observed.owner(), cpu);
        assert_eq!(observed.counter(), 4);
        assert_eq!(observed.generation(), 23);
    }

    #[test]
    fn close_request_remains_visible_to_the_switch_out_owner() {
        let cpu = PerfCpuId::new(3);
        let mut state = PmuRunState::new();
        let arm = state.begin_arm(cpu, TEST_COUNTER).unwrap();
        state.finish_arm(arm);

        let PmuCloseAction::Stop(lease) = state.begin_close() else {
            panic!("running event must request an owner-CPU stop");
        };
        assert_eq!(
            state.running(),
            Some(lease),
            "switch-out must still claim a close-requested hardware generation"
        );
        assert_eq!(state.claim_schedule_out(), Some(lease));
        state.finish_owner_stop(lease);
        assert_eq!(
            state.claim_requested_stop(lease),
            PmuStopClaim::AlreadyComplete,
            "the affine worker must treat a switch-out winner as a completed fence"
        );
        assert!(state.is_stopping());
    }

    #[test]
    fn disable_stops_one_generation_without_closing_the_event() {
        let cpu = PerfCpuId::new(1);
        let mut state = PmuRunState::new();
        let arm = state.begin_arm(cpu, TEST_COUNTER).unwrap();
        state.finish_arm(arm);

        let PmuCloseAction::Stop(lease) = state.begin_disable() else {
            panic!("disable must fence the active generation");
        };
        assert_eq!(
            state.claim_requested_stop(lease),
            PmuStopClaim::Claimed(lease)
        );
        state.finish_owner_stop(lease);
        assert!(!state.is_stopping());
        assert!(
            state.begin_arm(cpu, TEST_COUNTER).is_some(),
            "disable must permit re-enable"
        );
    }

    #[test]
    fn failed_owner_stop_can_be_claimed_again() {
        let cpu = PerfCpuId::new(2);
        let mut state = PmuRunState::new();
        let arm = state.begin_arm(cpu, TEST_COUNTER).unwrap();
        state.finish_arm(arm);

        let PmuCloseAction::Stop(lease) = state.begin_close() else {
            panic!("close must fence the active generation");
        };
        assert_eq!(
            state.claim_requested_stop(lease),
            PmuStopClaim::Claimed(lease)
        );

        // Model a fixed-CPU worker that claimed the stop but could not complete the
        // architecture operation. Teardown must retain the exact generation and
        // permit a later fd/task release to retry it.
        state.abort_owner_stop(lease);
        assert_eq!(
            state.claim_requested_stop(lease),
            PmuStopClaim::Claimed(lease)
        );
    }

    #[test]
    fn close_upgrades_an_in_flight_disable_to_permanent_teardown() {
        let cpu = PerfCpuId::new(4);
        let mut state = PmuRunState::new();
        let arm = state.begin_arm(cpu, TEST_COUNTER).unwrap();
        state.finish_arm(arm);

        let PmuCloseAction::Stop(lease) = state.begin_disable() else {
            panic!("disable must fence the active generation");
        };
        assert_eq!(
            state.claim_requested_stop(lease),
            PmuStopClaim::Claimed(lease)
        );
        assert_eq!(state.begin_close(), PmuCloseAction::Stop(lease));
        state.finish_owner_stop(lease);

        assert!(state.is_stopping());
        assert_eq!(state.begin_close(), PmuCloseAction::AlreadyClosed);
    }

    #[test]
    fn a_stale_lease_cannot_stop_the_next_arm_generation() {
        let cpu = PerfCpuId::new(5);
        let mut state = PmuRunState::new();
        let first_arm = state.begin_arm(cpu, TEST_COUNTER).unwrap();
        state.finish_arm(first_arm);
        let PmuCloseAction::Stop(first) = state.begin_disable() else {
            panic!("first disable must return its lease");
        };
        assert_eq!(
            state.claim_requested_stop(first),
            PmuStopClaim::Claimed(first)
        );
        state.finish_owner_stop(first);

        let second_arm = state.begin_arm(cpu, TEST_COUNTER).unwrap();
        state.finish_arm(second_arm);
        assert_eq!(state.claim_requested_stop(first), PmuStopClaim::Stale);
    }
}
