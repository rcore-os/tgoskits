use ax_task::runtime::{MonotonicDeadline, MonotonicInstant};

/// Absolute finite deadline accepted by the physical clockevent.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ClockDeadline(MonotonicDeadline);

impl ClockDeadline {
    pub(crate) const fn from_nanos(deadline_ns: u64) -> Option<Self> {
        match MonotonicDeadline::from_nanos(deadline_ns) {
            Some(deadline) => Some(Self(deadline)),
            None => None,
        }
    }

    pub(crate) const fn from_monotonic(deadline: MonotonicDeadline) -> Self {
        Self(deadline)
    }

    pub(crate) const fn as_nanos(self) -> u64 {
        self.0.as_nanos()
    }

    pub(crate) const fn as_monotonic(self) -> MonotonicDeadline {
        self.0
    }
}

/// Lifecycle of the physical clockevent owned by the current CPU.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClockEventPhase {
    Offline,
    Idle,
    Armed,
    Firing,
    /// An expired physical edge remains owned until IRQ-return rearm.
    #[cfg(feature = "multitask")]
    Deferred,
}

/// Hardware action produced by one clockevent state transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClockEventAction {
    None,
    Stop,
    Resume(ClockDeadline),
    Program(ClockDeadline),
}

/// Physical rearm ownership selected by the runtime integration layer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClockEventRearm {
    /// Reconcile the next edge in the firing transaction itself.
    #[cfg(any(test, not(feature = "multitask")))]
    Immediate,
    /// Transfer rearm to the IRQ-return or scheduler-tail transaction.
    #[cfg(feature = "multitask")]
    Deferred,
}

/// Move-only proof that one CPU lifecycle epoch owns a firing transaction.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct ClockEventFiringToken {
    cpu_epoch: u64,
    quiesce: ClockEventAction,
}

impl ClockEventFiringToken {
    #[cfg(feature = "irq")]
    pub(crate) const fn quiesce_action(&self) -> ClockEventAction {
        self.quiesce
    }
}

/// Result of claiming a physical timer interrupt edge.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum ClockEventIrqClaim {
    /// The edge has no live clockevent owner in this CPU epoch.
    Ignored,
    /// The armed epoch owns one bounded scheduler service transaction.
    Firing(ClockEventFiringToken),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchedulerTickState {
    Running { next: ClockDeadline },
    Stopped { resume_from: Option<ClockDeadline> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ClockEventDeviceState {
    Stopped,
    Started,
}

/// Single owner for every source merged into one physical per-CPU clockevent.
#[derive(Debug)]
pub(crate) struct LocalClockEvent {
    phase: ClockEventPhase,
    cpu_epoch: u64,
    #[cfg(feature = "multitask")]
    scheduler_generation: u64,
    #[cfg(feature = "multitask")]
    scheduler_deadline: Option<ClockDeadline>,
    scheduler_tick: SchedulerTickState,
    armed_deadline: Option<ClockDeadline>,
    device_state: ClockEventDeviceState,
}

impl LocalClockEvent {
    pub(crate) const fn offline() -> Self {
        Self {
            phase: ClockEventPhase::Offline,
            cpu_epoch: 0,
            #[cfg(feature = "multitask")]
            scheduler_generation: 0,
            #[cfg(feature = "multitask")]
            scheduler_deadline: None,
            scheduler_tick: SchedulerTickState::Stopped { resume_from: None },
            armed_deadline: None,
            device_state: ClockEventDeviceState::Stopped,
        }
    }

    pub(crate) fn online(&mut self, periodic: ClockDeadline) -> ClockEventAction {
        assert_eq!(
            self.phase,
            ClockEventPhase::Offline,
            "clockevent online transition requires the offline phase"
        );
        self.advance_cpu_epoch();
        self.scheduler_tick = SchedulerTickState::Running { next: periodic };
        self.phase = ClockEventPhase::Idle;
        self.reconcile_arm()
    }

    /// Stops the periodic scheduler tick before the owner CPU commits idle.
    #[cfg(feature = "multitask")]
    pub(crate) fn stop_scheduler_tick_for_idle(&mut self) -> ClockEventAction {
        assert!(!matches!(
            self.phase,
            ClockEventPhase::Offline | ClockEventPhase::Firing | ClockEventPhase::Deferred
        ));
        let SchedulerTickState::Running { next } = self.scheduler_tick else {
            return ClockEventAction::None;
        };
        self.scheduler_tick = SchedulerTickState::Stopped {
            resume_from: Some(next),
        };
        self.reconcile_arm()
    }

    /// Restarts the scheduler tick after idle work becomes runnable.
    #[cfg(feature = "multitask")]
    pub(crate) fn restart_scheduler_tick_after_idle(
        &mut self,
        now: MonotonicInstant,
        interval_ns: u64,
    ) -> ClockEventAction {
        assert!(!matches!(
            self.phase,
            ClockEventPhase::Offline | ClockEventPhase::Firing | ClockEventPhase::Deferred
        ));
        let SchedulerTickState::Stopped { resume_from } = self.scheduler_tick else {
            return ClockEventAction::None;
        };
        let next = match resume_from {
            Some(previous) => {
                crate::clock_event_runtime::next_periodic_deadline(previous, now, interval_ns)
            }
            None => crate::clock_event_runtime::initial_periodic_deadline(now, interval_ns),
        };
        self.scheduler_tick = SchedulerTickState::Running { next };
        self.reconcile_arm()
    }

    pub(crate) fn take_offline(&mut self) -> ClockEventAction {
        if self.phase == ClockEventPhase::Offline {
            return ClockEventAction::None;
        }
        let must_stop = self.device_state == ClockEventDeviceState::Started;
        self.advance_cpu_epoch();
        self.phase = ClockEventPhase::Offline;
        #[cfg(feature = "multitask")]
        {
            self.scheduler_deadline = None;
        }
        self.scheduler_tick = SchedulerTickState::Stopped { resume_from: None };
        self.armed_deadline = None;
        self.device_state = ClockEventDeviceState::Stopped;
        if must_stop {
            ClockEventAction::Stop
        } else {
            ClockEventAction::None
        }
    }

    #[cfg(feature = "multitask")]
    pub(crate) fn publish_scheduler(
        &mut self,
        generation: u64,
        deadline: Option<MonotonicDeadline>,
    ) -> ClockEventAction {
        if generation <= self.scheduler_generation {
            return ClockEventAction::None;
        }
        self.scheduler_generation = generation;
        self.scheduler_deadline = deadline.map(ClockDeadline::from_monotonic);
        self.reconcile_arm()
    }

    /// Claims a physical timer edge for this CPU lifecycle epoch.
    ///
    /// Every edge observed while armed starts one hrtimer-style firing
    /// transaction. Logical expiry is decided by the scheduler using its own
    /// clock; an early or stale hardware edge therefore completes normally
    /// and reprograms the still-earliest absolute deadline exactly once.
    pub(crate) fn claim_irq(&mut self, _now: MonotonicInstant) -> ClockEventIrqClaim {
        match self.phase {
            ClockEventPhase::Offline | ClockEventPhase::Idle | ClockEventPhase::Firing => {
                return ClockEventIrqClaim::Ignored;
            }
            #[cfg(feature = "multitask")]
            ClockEventPhase::Deferred => {
                return ClockEventIrqClaim::Ignored;
            }
            ClockEventPhase::Armed => {}
        }
        let _armed = self
            .armed_deadline
            .expect("armed clockevent must retain its physical deadline");
        assert_eq!(
            self.device_state,
            ClockEventDeviceState::Started,
            "an armed clockevent must own an observable physical source"
        );
        self.armed_deadline = None;
        self.device_state = ClockEventDeviceState::Stopped;
        self.phase = ClockEventPhase::Firing;
        ClockEventIrqClaim::Firing(ClockEventFiringToken {
            cpu_epoch: self.cpu_epoch,
            quiesce: ClockEventAction::Stop,
        })
    }

    /// Advances periodic accounting without producing a scheduling decision.
    ///
    /// A periodic clockevent is only one physical wakeup source. Whether the
    /// current thread must be preempted remains an ax-task policy decision.
    pub(crate) fn advance_periodic(&mut self, now: MonotonicInstant, interval_ns: u64) -> bool {
        let SchedulerTickState::Running { next } = &mut self.scheduler_tick else {
            return false;
        };
        let current = *next;
        if !now.reached(current.as_monotonic()) {
            return false;
        }
        *next = crate::clock_event_runtime::next_periodic_deadline(current, now, interval_ns);
        true
    }

    pub(crate) fn finish_firing(
        &mut self,
        token: ClockEventFiringToken,
        rearm: ClockEventRearm,
    ) -> ClockEventAction {
        if token.cpu_epoch != self.cpu_epoch {
            return ClockEventAction::None;
        }
        assert_eq!(
            self.phase,
            ClockEventPhase::Firing,
            "clockevent finish requires a firing transaction"
        );
        match rearm {
            #[cfg(any(test, not(feature = "multitask")))]
            ClockEventRearm::Immediate => {
                self.phase = ClockEventPhase::Idle;
                self.reconcile_arm()
            }
            #[cfg(feature = "multitask")]
            ClockEventRearm::Deferred => {
                // Match Linux's TIF_HRTIMER_REARM transaction: the expired
                // comparator has left the active base while IRQs stay
                // disabled. Scheduler/IRQ return reconciles the latest logical
                // minimum exactly once before enabling IRQs.
                self.phase = ClockEventPhase::Deferred;
                ClockEventAction::None
            }
        }
    }

    /// Completes a deferred firing transaction before local IRQs are enabled.
    #[cfg(feature = "multitask")]
    pub(crate) fn finish_deferred_rearm(&mut self) -> ClockEventAction {
        if self.phase != ClockEventPhase::Deferred {
            return ClockEventAction::None;
        }
        self.phase = ClockEventPhase::Idle;
        self.reconcile_arm()
    }

    #[cfg(test)]
    pub(crate) const fn phase(&self) -> ClockEventPhase {
        self.phase
    }

    #[cfg(test)]
    pub(crate) const fn cpu_epoch(&self) -> u64 {
        self.cpu_epoch
    }

    #[cfg(all(test, feature = "multitask"))]
    pub(crate) const fn scheduler_generation(&self) -> u64 {
        self.scheduler_generation
    }

    #[cfg(all(test, feature = "multitask"))]
    pub(crate) const fn scheduler_deadline(&self) -> Option<ClockDeadline> {
        self.scheduler_deadline
    }

    #[cfg(test)]
    pub(crate) const fn armed_deadline(&self) -> Option<ClockDeadline> {
        self.armed_deadline
    }

    #[cfg(feature = "multitask")]
    pub(crate) fn has_immediate_work(&self, now: MonotonicInstant) -> bool {
        self.selected_deadline()
            .is_some_and(|deadline| now.reached(deadline.as_monotonic()))
    }

    fn selected_deadline(&self) -> Option<ClockDeadline> {
        #[cfg(feature = "multitask")]
        {
            let scheduler_tick = match self.scheduler_tick {
                SchedulerTickState::Running { next } => Some(next),
                SchedulerTickState::Stopped { .. } => None,
            };
            match (scheduler_tick, self.scheduler_deadline) {
                (Some(periodic), Some(task)) => Some(periodic.min(task)),
                (Some(periodic), None) => Some(periodic),
                (None, Some(task)) => Some(task),
                (None, None) => None,
            }
        }
        #[cfg(not(feature = "multitask"))]
        {
            match self.scheduler_tick {
                SchedulerTickState::Running { next } => Some(next),
                SchedulerTickState::Stopped { .. } => None,
            }
        }
    }

    fn reconcile_arm(&mut self) -> ClockEventAction {
        match self.phase {
            ClockEventPhase::Offline | ClockEventPhase::Firing => {
                return ClockEventAction::None;
            }
            #[cfg(feature = "multitask")]
            ClockEventPhase::Deferred => return ClockEventAction::None,
            ClockEventPhase::Idle | ClockEventPhase::Armed => {}
        }
        let selected = self.selected_deadline();
        if let Some(armed) = self.armed_deadline {
            // The physical comparator represents the current logical minimum,
            // not the task which happened to arm it. Linux hrtick restarts a
            // queued timer when a context switch moves the active request in
            // either direction. Keeping the previous task's later-invalidated
            // slice edge would turn every blocking switch into a stale IRQ.
            //
            // Local IRQ exclusion serializes this state transition with the
            // handler. A residual hardware edge which raced the reprogram is
            // merely claimed as an early edge and reconciled once more.
            match selected {
                Some(deadline) if deadline == armed => return ClockEventAction::None,
                Some(deadline) => {
                    self.armed_deadline = Some(deadline);
                    self.phase = ClockEventPhase::Armed;
                    return ClockEventAction::Program(deadline);
                }
                None => {
                    self.armed_deadline = None;
                    self.phase = ClockEventPhase::Idle;
                    self.device_state = ClockEventDeviceState::Stopped;
                    return ClockEventAction::Stop;
                }
            }
        }

        match selected {
            Some(deadline) => {
                self.armed_deadline = Some(deadline);
                self.phase = ClockEventPhase::Armed;
                if self.device_state == ClockEventDeviceState::Stopped {
                    self.device_state = ClockEventDeviceState::Started;
                    ClockEventAction::Resume(deadline)
                } else {
                    ClockEventAction::Program(deadline)
                }
            }
            None => {
                self.phase = ClockEventPhase::Idle;
                if self.device_state == ClockEventDeviceState::Started {
                    self.device_state = ClockEventDeviceState::Stopped;
                    ClockEventAction::Stop
                } else {
                    ClockEventAction::None
                }
            }
        }
    }

    fn advance_cpu_epoch(&mut self) {
        self.cpu_epoch = self
            .cpu_epoch
            .checked_add(1)
            .expect("clockevent CPU lifecycle epoch exhausted");
    }
}

#[cfg(all(test, not(feature = "multitask")))]
mod single_task_tests {
    use ax_task::runtime::MonotonicInstant;

    use super::{
        ClockDeadline, ClockEventAction, ClockEventIrqClaim, ClockEventPhase, ClockEventRearm,
        LocalClockEvent,
    };

    fn deadline(nanos: u64) -> ClockDeadline {
        ClockDeadline::from_nanos(nanos).unwrap()
    }

    fn instant(nanos: u64) -> MonotonicInstant {
        MonotonicInstant::from_nanos(nanos).unwrap()
    }

    #[test]
    fn host_clockevent_exercises_the_single_task_lifecycle() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(event.phase(), ClockEventPhase::Offline);
        assert_eq!(event.cpu_epoch(), 0);
        assert_eq!(event.armed_deadline(), None);
        assert_eq!(event.claim_irq(instant(0)), ClockEventIrqClaim::Ignored);

        assert_eq!(
            event.online(deadline(100)),
            ClockEventAction::Resume(deadline(100))
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
        assert_eq!(event.cpu_epoch(), 1);
        assert_eq!(event.armed_deadline(), Some(deadline(100)));
        assert!(event.advance_periodic(instant(100), 25));

        let firing = match event.claim_irq(instant(100)) {
            ClockEventIrqClaim::Firing(firing) => firing,
            claim => panic!("armed clockevent was not claimed: {claim:?}"),
        };
        assert_eq!(event.phase(), ClockEventPhase::Firing);
        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Immediate),
            ClockEventAction::Resume(deadline(125))
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
        assert_eq!(event.armed_deadline(), Some(deadline(125)));

        assert_eq!(event.take_offline(), ClockEventAction::Stop);
        assert_eq!(event.take_offline(), ClockEventAction::None);
        assert_eq!(event.phase(), ClockEventPhase::Offline);
        assert_eq!(event.armed_deadline(), None);
    }
}

#[cfg(all(test, feature = "multitask"))]
mod tests {
    use ax_task::runtime::{MonotonicDeadline, MonotonicInstant};

    use super::{
        ClockDeadline, ClockEventAction, ClockEventDeviceState, ClockEventFiringToken,
        ClockEventIrqClaim, ClockEventPhase, ClockEventRearm, LocalClockEvent,
    };

    fn deadline(nanos: u64) -> ClockDeadline {
        ClockDeadline::from_nanos(nanos).unwrap()
    }

    fn instant(nanos: u64) -> MonotonicInstant {
        MonotonicInstant::from_nanos(nanos).unwrap()
    }

    fn scheduler_deadline(nanos: u64) -> MonotonicDeadline {
        MonotonicDeadline::from_nanos(nanos).unwrap()
    }

    fn fire_due(event: &mut LocalClockEvent, now_ns: u64) -> ClockEventFiringToken {
        match event.claim_irq(instant(now_ns)) {
            ClockEventIrqClaim::Firing(token) => token,
            claim => panic!("due clockevent was not claimed: {claim:?}"),
        }
    }

    #[test]
    fn values_outside_linux_ktime_are_not_physical_deadlines() {
        assert_eq!(ClockDeadline::from_nanos(u64::MAX), None);
        assert_eq!(
            ClockDeadline::from_nanos(ax_task::runtime::KTIME_MAX_NANOS),
            Some(deadline(ax_task::runtime::KTIME_MAX_NANOS))
        );
    }

    #[test]
    fn zero_is_a_valid_already_due_physical_deadline() {
        assert_eq!(ClockDeadline::from_nanos(0), Some(deadline(0)));
    }

    #[test]
    fn offline_stops_the_device_and_allows_a_fresh_online_cycle() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(100)),
            ClockEventAction::Resume(deadline(100))
        );
        assert_eq!(event.take_offline(), ClockEventAction::Stop);
        assert_eq!(event.phase(), ClockEventPhase::Offline);
        assert_eq!(event.armed_deadline(), None);
        assert_eq!(
            event.online(deadline(200)),
            ClockEventAction::Resume(deadline(200))
        );
    }

    #[test]
    fn stale_irq_after_reonline_cannot_fire_the_new_cpu_epoch_early() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(100)),
            ClockEventAction::Resume(deadline(100))
        );
        assert_eq!(event.take_offline(), ClockEventAction::Stop);
        assert_eq!(
            event.online(deadline(200)),
            ClockEventAction::Resume(deadline(200))
        );

        // A stale physical edge enters the same firing transaction as any
        // other hrtimer edge. Since no logical deadline is due, finish
        // reprograms the new epoch's still-earliest deadline.
        let firing = match event.claim_irq(instant(150)) {
            ClockEventIrqClaim::Firing(firing) => firing,
            claim => panic!("armed edge was not claimed: {claim:?}"),
        };
        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Immediate),
            ClockEventAction::Resume(deadline(200))
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
        assert_eq!(event.armed_deadline(), Some(deadline(200)));
    }

    #[test]
    fn old_firing_token_cannot_commit_across_an_offline_cycle() {
        let mut event = LocalClockEvent::offline();
        event.online(deadline(100));
        let old_epoch = event.cpu_epoch();
        let firing = fire_due(&mut event, 100);

        assert_eq!(event.take_offline(), ClockEventAction::None);
        event.online(deadline(200));
        assert!(event.cpu_epoch() > old_epoch);

        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Immediate),
            ClockEventAction::None
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
        assert_eq!(event.armed_deadline(), Some(deadline(200)));
    }

    #[test]
    fn idle_entry_removes_the_scheduler_tick_from_physical_selection() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(100)),
            ClockEventAction::Resume(deadline(100))
        );
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(1_000))),
            ClockEventAction::None
        );

        assert_eq!(
            event.stop_scheduler_tick_for_idle(),
            ClockEventAction::Program(deadline(1_000))
        );
        assert_eq!(event.armed_deadline(), Some(deadline(1_000)));
    }

    #[test]
    fn idle_exit_restarts_the_tick_on_its_original_phase() {
        let mut event = LocalClockEvent::offline();
        event.online(deadline(100));
        assert_eq!(event.stop_scheduler_tick_for_idle(), ClockEventAction::Stop);

        assert_eq!(
            event.restart_scheduler_tick_after_idle(instant(149), 25),
            ClockEventAction::Resume(deadline(150))
        );
        assert_eq!(event.armed_deadline(), Some(deadline(150)));
    }

    #[test]
    fn repeated_idle_iteration_keeps_the_scheduler_tick_stopped() {
        let mut event = LocalClockEvent::offline();
        event.online(deadline(100));
        assert_eq!(event.stop_scheduler_tick_for_idle(), ClockEventAction::Stop);
        assert_eq!(event.stop_scheduler_tick_for_idle(), ClockEventAction::None);
        assert_eq!(event.phase(), ClockEventPhase::Idle);
        assert_eq!(event.armed_deadline(), None);
    }

    #[test]
    fn stale_scheduler_generation_cannot_cross_an_offline_cycle() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.publish_scheduler(7, Some(scheduler_deadline(90))),
            ClockEventAction::None
        );
        assert_eq!(
            event.online(deadline(100)),
            ClockEventAction::Resume(deadline(90))
        );
        assert_eq!(event.take_offline(), ClockEventAction::Stop);
        assert_eq!(
            event.publish_scheduler(6, Some(scheduler_deadline(50))),
            ClockEventAction::None
        );
        assert_eq!(
            event.online(deadline(200)),
            ClockEventAction::Resume(deadline(200))
        );
        assert_eq!(event.scheduler_generation(), 7);
        assert_eq!(event.scheduler_deadline(), None);
    }

    #[test]
    #[should_panic(expected = "finite monotonic clock domain")]
    fn periodic_overflow_is_a_fatal_clock_domain_violation() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(ax_task::runtime::KTIME_MAX_NANOS - 5)),
            ClockEventAction::Resume(deadline(ax_task::runtime::KTIME_MAX_NANOS - 5))
        );
        let _firing = fire_due(&mut event, ax_task::runtime::KTIME_MAX_NANOS - 1);
        assert!(event.advance_periodic(instant(ax_task::runtime::KTIME_MAX_NANOS - 1), 10));
    }

    #[test]
    fn later_live_scheduler_deadline_replaces_a_future_slice_edge() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(500)),
            ClockEventAction::Resume(deadline(500))
        );
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(300))),
            ClockEventAction::Program(deadline(300))
        );
        assert!(
            !event.has_immediate_work(instant(250)),
            "the obsolete slice edge must still be reprogrammable"
        );

        assert_eq!(
            event.publish_scheduler(2, Some(scheduler_deadline(400))),
            ClockEventAction::Program(deadline(400)),
            "a new current task must replace the previous task's future slice edge"
        );
        assert_eq!(event.armed_deadline(), Some(deadline(400)));
    }

    #[test]
    fn logical_minimum_reprograms_when_scheduler_deadlines_move_later() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(500)),
            ClockEventAction::Resume(deadline(500))
        );
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(300))),
            ClockEventAction::Program(deadline(300))
        );
        assert_eq!(
            event.publish_scheduler(2, Some(scheduler_deadline(400))),
            ClockEventAction::Program(deadline(400))
        );
        assert_eq!(
            event.publish_scheduler(3, None),
            ClockEventAction::Program(deadline(500))
        );
        assert_eq!(event.armed_deadline(), Some(deadline(500)));
    }

    #[test]
    fn residual_edge_after_later_reprogram_is_reconciled_as_early() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(500)),
            ClockEventAction::Resume(deadline(500))
        );
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(300))),
            ClockEventAction::Program(deadline(300))
        );
        assert!(
            event.has_immediate_work(instant(350)),
            "the programmed comparator is already elapsed before IRQ entry"
        );

        assert_eq!(
            event.publish_scheduler(2, Some(scheduler_deadline(400))),
            ClockEventAction::Program(deadline(400)),
            "the elapsed edge no longer represents a live logical deadline"
        );
        assert_eq!(event.armed_deadline(), Some(deadline(400)));

        // A backend may still deliver the old pending edge after the
        // comparator was moved. It is an early firing for the new logical
        // minimum and must rearm that same deadline without consuming it.
        let firing = fire_due(&mut event, 350);
        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Immediate),
            ClockEventAction::Resume(deadline(400))
        );
        assert_eq!(event.armed_deadline(), Some(deadline(400)));
    }

    #[test]
    fn stopped_to_armed_and_reprogram_are_distinct_device_actions() {
        let mut event = LocalClockEvent::offline();
        let start = event.online(deadline(500));
        let reprogram = event.publish_scheduler(1, Some(scheduler_deadline(300)));

        assert_eq!(start, ClockEventAction::Resume(deadline(500)));
        assert_eq!(reprogram, ClockEventAction::Program(deadline(300)));
    }

    #[test]
    fn removing_the_only_deadline_stops_the_obsolete_physical_edge() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(1_000)),
            ClockEventAction::Resume(deadline(1_000))
        );
        assert_eq!(event.stop_scheduler_tick_for_idle(), ClockEventAction::Stop);
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(300))),
            ClockEventAction::Resume(deadline(300))
        );
        assert_eq!(event.publish_scheduler(2, None), ClockEventAction::Stop);
        assert_eq!(event.phase(), ClockEventPhase::Idle);
        assert_eq!(event.armed_deadline(), None);
        assert_eq!(event.claim_irq(instant(300)), ClockEventIrqClaim::Ignored);
    }

    #[test]
    fn stale_generation_is_ignored() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.publish_scheduler(7, Some(scheduler_deadline(200))),
            ClockEventAction::None
        );
        assert_eq!(
            event.publish_scheduler(6, Some(scheduler_deadline(100))),
            ClockEventAction::None
        );
        assert_eq!(event.scheduler_generation(), 7);
        assert_eq!(event.scheduler_deadline(), Some(deadline(200)));
    }

    #[test]
    fn firing_merges_updates_and_programs_exactly_once_at_finish() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(500)),
            ClockEventAction::Resume(deadline(500))
        );
        let firing = fire_due(&mut event, 500);
        assert_eq!(event.phase(), ClockEventPhase::Firing);
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(450))),
            ClockEventAction::None
        );
        assert_eq!(
            event.publish_scheduler(2, Some(scheduler_deadline(250))),
            ClockEventAction::None
        );
        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Immediate),
            ClockEventAction::Resume(deadline(250))
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
    }

    #[test]
    fn periodic_advance_is_merged_with_scheduler_deadline() {
        let mut event = LocalClockEvent::offline();
        event.online(deadline(100));
        let firing = fire_due(&mut event, 100);
        assert!(event.advance_periodic(instant(100), 25));
        event.publish_scheduler(1, Some(scheduler_deadline(140)));
        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Immediate),
            ClockEventAction::Resume(deadline(125))
        );
    }

    #[test]
    fn simultaneous_periodic_and_scheduler_expiry_programs_one_replacement() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(100)),
            ClockEventAction::Resume(deadline(100))
        );
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(100))),
            ClockEventAction::None
        );

        let firing = fire_due(&mut event, 100);
        assert!(event.advance_periodic(instant(100), 25));
        assert_eq!(event.publish_scheduler(2, None), ClockEventAction::None);

        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Immediate),
            ClockEventAction::Resume(deadline(125))
        );
        assert_eq!(event.armed_deadline(), Some(deadline(125)));
    }

    #[test]
    fn deferred_rearm_keeps_the_physical_edge_owned_until_irq_return() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(500)),
            ClockEventAction::Resume(deadline(500))
        );
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(100))),
            ClockEventAction::Program(deadline(100))
        );

        let firing = fire_due(&mut event, 100);
        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Deferred),
            ClockEventAction::None,
            "multitask IRQ return must retain a deferred physical rearm owner"
        );
        assert_eq!(event.phase(), ClockEventPhase::Deferred);
        assert_eq!(event.armed_deadline(), None);

        assert_eq!(
            event.publish_scheduler(2, Some(scheduler_deadline(400))),
            ClockEventAction::None,
            "logical publication cannot commit hardware before scheduler tail"
        );
        assert_eq!(event.phase(), ClockEventPhase::Deferred);
        assert_eq!(event.armed_deadline(), None);
        assert_eq!(
            event.finish_deferred_rearm(),
            ClockEventAction::Resume(deadline(400))
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
        assert_eq!(event.armed_deadline(), Some(deadline(400)));
    }

    #[test]
    fn firing_quiesces_the_level_source_before_deferred_rearm() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(100)),
            ClockEventAction::Resume(deadline(100))
        );

        let firing = fire_due(&mut event, 100);
        assert_eq!(
            event.device_state,
            ClockEventDeviceState::Stopped,
            "an acknowledged level clockevent must be unobservable before controller EOI"
        );
        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Deferred),
            ClockEventAction::None
        );
        assert_eq!(
            event.finish_deferred_rearm(),
            ClockEventAction::Resume(deadline(100)),
            "deferred rearm must reactivate the quiesced source"
        );
    }

    #[test]
    fn early_irq_uses_one_firing_transaction_and_reprograms_once() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(500)),
            ClockEventAction::Resume(deadline(500))
        );
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(100))),
            ClockEventAction::Program(deadline(100))
        );

        let firing = match event.claim_irq(instant(50)) {
            ClockEventIrqClaim::Firing(firing) => firing,
            claim => panic!("armed edge was not claimed: {claim:?}"),
        };
        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Immediate),
            ClockEventAction::Resume(deadline(100))
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
        assert_eq!(event.armed_deadline(), Some(deadline(100)));
    }

    #[test]
    fn idle_entry_stops_the_edge_and_ignores_a_residual_irq() {
        let mut event = LocalClockEvent::offline();
        event.online(deadline(500));
        assert_eq!(event.stop_scheduler_tick_for_idle(), ClockEventAction::Stop);
        assert_eq!(event.phase(), ClockEventPhase::Idle);
        assert_eq!(event.armed_deadline(), None);
        assert_eq!(event.claim_irq(instant(100)), ClockEventIrqClaim::Ignored);
    }

    #[test]
    fn overdue_scheduler_deadline_remains_immediate_until_firing_reconciles_it() {
        let mut event = LocalClockEvent::offline();
        event.online(deadline(500));
        assert_eq!(
            event.publish_scheduler(1, Some(scheduler_deadline(90))),
            ClockEventAction::Program(deadline(90))
        );
        assert!(event.has_immediate_work(instant(100)));

        let firing = fire_due(&mut event, 100);
        assert_eq!(event.publish_scheduler(2, None), ClockEventAction::None);
        assert_eq!(
            event.finish_firing(firing, ClockEventRearm::Immediate),
            ClockEventAction::Resume(deadline(500))
        );
        assert!(!event.has_immediate_work(instant(100)));
    }
}
