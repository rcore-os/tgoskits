/// Absolute finite, non-zero deadline accepted by the physical clockevent.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ClockDeadline(u64);

impl ClockDeadline {
    pub(crate) const fn from_nanos(deadline_ns: u64) -> Option<Self> {
        if deadline_ns == 0 || deadline_ns == u64::MAX {
            None
        } else {
            Some(Self(deadline_ns))
        }
    }

    pub(crate) const fn as_nanos(self) -> u64 {
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
}

/// Hardware action produced by one clockevent state transition.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ClockEventAction {
    None,
    Stop,
    Program(ClockDeadline),
}

/// Single owner for every source merged into one physical per-CPU clockevent.
#[derive(Debug)]
pub(crate) struct LocalClockEvent {
    phase: ClockEventPhase,
    #[cfg(feature = "multitask")]
    task_generation: u64,
    #[cfg(feature = "multitask")]
    task_deadline: Option<ClockDeadline>,
    periodic_deadline: Option<ClockDeadline>,
    armed_deadline: Option<ClockDeadline>,
    #[cfg(feature = "multitask")]
    deferred_work: bool,
}

impl LocalClockEvent {
    pub(crate) const fn offline() -> Self {
        Self {
            phase: ClockEventPhase::Offline,
            #[cfg(feature = "multitask")]
            task_generation: 0,
            #[cfg(feature = "multitask")]
            task_deadline: None,
            periodic_deadline: None,
            armed_deadline: None,
            #[cfg(feature = "multitask")]
            deferred_work: false,
        }
    }

    pub(crate) fn online(&mut self, periodic: Option<ClockDeadline>) -> ClockEventAction {
        assert_eq!(
            self.phase,
            ClockEventPhase::Offline,
            "clockevent online transition requires the offline phase"
        );
        self.periodic_deadline = periodic;
        self.phase = ClockEventPhase::Idle;
        self.reconcile_arm()
    }

    pub(crate) fn take_offline(&mut self) -> ClockEventAction {
        if self.phase == ClockEventPhase::Offline {
            return ClockEventAction::None;
        }
        let must_stop = self.armed_deadline.is_some() || self.phase == ClockEventPhase::Firing;
        self.phase = ClockEventPhase::Offline;
        #[cfg(feature = "multitask")]
        {
            self.task_deadline = None;
            self.deferred_work = false;
        }
        self.periodic_deadline = None;
        self.armed_deadline = None;
        if must_stop {
            ClockEventAction::Stop
        } else {
            ClockEventAction::None
        }
    }

    #[cfg(feature = "multitask")]
    pub(crate) fn publish_task(
        &mut self,
        generation: u64,
        deadline_ns: Option<u64>,
        deferred_work: bool,
    ) -> ClockEventAction {
        if generation <= self.task_generation {
            return ClockEventAction::None;
        }
        self.task_generation = generation;
        self.task_deadline = deadline_ns.and_then(ClockDeadline::from_nanos);
        self.deferred_work = deferred_work;
        self.reconcile_arm()
    }

    pub(crate) fn begin_firing(&mut self) {
        assert_ne!(
            self.phase,
            ClockEventPhase::Offline,
            "offline CPU received a clockevent"
        );
        self.armed_deadline = None;
        self.phase = ClockEventPhase::Firing;
    }

    /// Claims an armed deadline that has already elapsed.
    ///
    /// Idle uses this with local IRQs disabled after its final pending-work
    /// check. A clockevent can reach zero without leaving a consumable IRQ
    /// edge, so merely refusing to sleep would otherwise livelock the idle
    /// loop forever on the stale `Armed` state.
    #[cfg(feature = "multitask")]
    pub(crate) fn begin_firing_if_due(&mut self, now_ns: u64) -> bool {
        if self.phase != ClockEventPhase::Armed
            || !self
                .armed_deadline
                .is_some_and(|deadline| deadline.as_nanos() <= now_ns)
        {
            return false;
        }
        self.armed_deadline = None;
        self.phase = ClockEventPhase::Firing;
        true
    }

    /// Advances periodic accounting without producing a scheduling decision.
    ///
    /// A periodic clockevent is only one physical wakeup source. Whether the
    /// current thread must be preempted remains an ax-task policy decision.
    pub(crate) fn advance_periodic(&mut self, now_ns: u64, interval_ns: u64) -> bool {
        let Some(current) = self.periodic_deadline else {
            return false;
        };
        if now_ns < current.as_nanos() {
            return false;
        }
        self.periodic_deadline = crate::clock_event_runtime::next_periodic_deadline(
            current.as_nanos(),
            now_ns,
            interval_ns,
        )
        .and_then(ClockDeadline::from_nanos);
        true
    }

    pub(crate) fn finish_firing(&mut self) -> ClockEventAction {
        assert_eq!(
            self.phase,
            ClockEventPhase::Firing,
            "clockevent finish requires a firing transaction"
        );
        self.recover_firing()
    }

    /// Restores an abandoned firing transaction and recomputes the next arm.
    ///
    /// The IRQ wrapper uses this from its unwind guard so a recoverable host
    /// panic cannot leave the per-CPU clockevent permanently in `Firing`.
    pub(crate) fn recover_firing(&mut self) -> ClockEventAction {
        if self.phase != ClockEventPhase::Firing {
            return ClockEventAction::None;
        }
        self.phase = ClockEventPhase::Idle;
        self.reconcile_arm()
    }

    #[cfg(test)]
    pub(crate) const fn phase(&self) -> ClockEventPhase {
        self.phase
    }

    #[cfg(all(test, feature = "multitask"))]
    pub(crate) const fn task_generation(&self) -> u64 {
        self.task_generation
    }

    #[cfg(all(test, feature = "multitask"))]
    pub(crate) const fn task_deadline(&self) -> Option<ClockDeadline> {
        self.task_deadline
    }

    #[cfg(test)]
    pub(crate) const fn armed_deadline(&self) -> Option<ClockDeadline> {
        self.armed_deadline
    }

    #[cfg(all(test, feature = "multitask"))]
    pub(crate) const fn deferred_work(&self) -> bool {
        self.deferred_work
    }

    #[cfg(feature = "multitask")]
    pub(crate) fn has_immediate_work(&self, now_ns: u64) -> bool {
        self.deferred_work
            || self
                .selected_deadline()
                .is_some_and(|deadline| deadline.as_nanos() <= now_ns)
    }

    fn selected_deadline(&self) -> Option<ClockDeadline> {
        #[cfg(feature = "multitask")]
        {
            match (self.periodic_deadline, self.task_deadline) {
                (Some(periodic), Some(task)) => Some(periodic.min(task)),
                (Some(periodic), None) => Some(periodic),
                (None, Some(task)) => Some(task),
                (None, None) => None,
            }
        }
        #[cfg(not(feature = "multitask"))]
        {
            self.periodic_deadline
        }
    }

    fn reconcile_arm(&mut self) -> ClockEventAction {
        if matches!(
            self.phase,
            ClockEventPhase::Offline | ClockEventPhase::Firing
        ) {
            return ClockEventAction::None;
        }
        let selected = self.selected_deadline();
        match (self.phase, self.armed_deadline, selected) {
            (ClockEventPhase::Idle, None, Some(deadline)) => {
                self.armed_deadline = Some(deadline);
                self.phase = ClockEventPhase::Armed;
                ClockEventAction::Program(deadline)
            }
            (ClockEventPhase::Armed, Some(armed), Some(deadline)) if deadline != armed => {
                self.armed_deadline = Some(deadline);
                ClockEventAction::Program(deadline)
            }
            (ClockEventPhase::Armed, Some(_), None) => {
                self.armed_deadline = None;
                self.phase = ClockEventPhase::Idle;
                ClockEventAction::Stop
            }
            (ClockEventPhase::Idle, None, None) | (ClockEventPhase::Armed, Some(_), Some(_)) => {
                ClockEventAction::None
            }
            (phase, armed, selected) => {
                panic!(
                    "invalid clockevent state: phase={phase:?}, armed={armed:?}, \
                     selected={selected:?}"
                );
            }
        }
    }
}

#[cfg(all(test, feature = "multitask"))]
mod tests {
    use super::{ClockDeadline, ClockEventAction, ClockEventPhase, LocalClockEvent};

    fn deadline(nanos: u64) -> ClockDeadline {
        ClockDeadline::from_nanos(nanos).unwrap()
    }

    #[test]
    fn numeric_infinity_is_not_a_physical_deadline() {
        assert_eq!(ClockDeadline::from_nanos(u64::MAX), None);
    }

    #[test]
    fn offline_stops_the_device_and_allows_a_fresh_online_cycle() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(Some(deadline(100))),
            ClockEventAction::Program(deadline(100))
        );
        assert_eq!(event.take_offline(), ClockEventAction::Stop);
        assert_eq!(event.phase(), ClockEventPhase::Offline);
        assert_eq!(event.armed_deadline(), None);
        assert_eq!(
            event.online(Some(deadline(200))),
            ClockEventAction::Program(deadline(200))
        );
    }

    #[test]
    fn online_without_a_finite_periodic_deadline_stays_idle() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(event.online(None), ClockEventAction::None);
        assert_eq!(event.phase(), ClockEventPhase::Idle);
        assert_eq!(event.armed_deadline(), None);
    }

    #[test]
    fn stale_task_generation_cannot_cross_an_offline_cycle() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.publish_task(7, Some(90), true),
            ClockEventAction::None
        );
        assert_eq!(
            event.online(Some(deadline(100))),
            ClockEventAction::Program(deadline(90))
        );
        assert_eq!(event.take_offline(), ClockEventAction::Stop);
        assert_eq!(
            event.publish_task(6, Some(50), true),
            ClockEventAction::None
        );
        assert_eq!(
            event.online(Some(deadline(200))),
            ClockEventAction::Program(deadline(200))
        );
        assert_eq!(event.task_generation(), 7);
        assert_eq!(event.task_deadline(), None);
        assert!(!event.deferred_work());
    }

    #[test]
    fn periodic_overflow_becomes_idle_instead_of_programming_infinity() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(Some(deadline(u64::MAX - 5))),
            ClockEventAction::Program(deadline(u64::MAX - 5))
        );
        event.begin_firing();
        assert!(event.advance_periodic(u64::MAX - 1, 10));
        assert_eq!(event.finish_firing(), ClockEventAction::None);
        assert_eq!(event.phase(), ClockEventPhase::Idle);
        assert_eq!(event.armed_deadline(), None);
    }

    #[test]
    fn every_selected_deadline_change_reprograms_the_physical_owner() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(Some(deadline(500))),
            ClockEventAction::Program(deadline(500))
        );
        assert_eq!(
            event.publish_task(1, Some(300), false),
            ClockEventAction::Program(deadline(300))
        );
        assert_eq!(
            event.publish_task(2, Some(400), false),
            ClockEventAction::Program(deadline(400))
        );
        assert_eq!(event.armed_deadline(), Some(deadline(400)));
        assert_eq!(
            event.publish_task(3, None, false),
            ClockEventAction::Program(deadline(500))
        );
        assert_eq!(event.armed_deadline(), Some(deadline(500)));
    }

    #[test]
    fn removing_the_only_deadline_stops_the_physical_owner() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(event.online(None), ClockEventAction::None);
        assert_eq!(
            event.publish_task(1, Some(300), false),
            ClockEventAction::Program(deadline(300))
        );
        assert_eq!(event.publish_task(2, None, false), ClockEventAction::Stop);
        assert_eq!(event.phase(), ClockEventPhase::Idle);
        assert_eq!(event.armed_deadline(), None);
    }

    #[test]
    fn stale_generation_is_ignored() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.publish_task(7, Some(200), false),
            ClockEventAction::None
        );
        assert_eq!(
            event.publish_task(6, Some(100), true),
            ClockEventAction::None
        );
        assert_eq!(event.task_generation(), 7);
        assert_eq!(event.task_deadline(), Some(deadline(200)));
        assert!(!event.deferred_work());
    }

    #[test]
    fn firing_merges_updates_and_programs_exactly_once_at_finish() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(Some(deadline(500))),
            ClockEventAction::Program(deadline(500))
        );
        event.begin_firing();
        assert_eq!(event.phase(), ClockEventPhase::Firing);
        assert_eq!(
            event.publish_task(1, Some(450), false),
            ClockEventAction::None
        );
        assert_eq!(
            event.publish_task(2, Some(250), true),
            ClockEventAction::None
        );
        assert_eq!(
            event.finish_firing(),
            ClockEventAction::Program(deadline(250))
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
        assert!(event.deferred_work());
    }

    #[test]
    fn periodic_advance_is_merged_with_task_deadline() {
        let mut event = LocalClockEvent::offline();
        event.online(Some(deadline(100)));
        event.begin_firing();
        assert!(event.advance_periodic(100, 25));
        event.publish_task(1, Some(140), false);
        assert_eq!(
            event.finish_firing(),
            ClockEventAction::Program(deadline(125))
        );
    }

    #[test]
    fn simultaneous_periodic_and_task_expiry_programs_one_replacement() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(Some(deadline(100))),
            ClockEventAction::Program(deadline(100))
        );
        assert_eq!(
            event.publish_task(1, Some(100), false),
            ClockEventAction::None
        );

        event.begin_firing();
        assert!(event.advance_periodic(100, 25));
        assert_eq!(event.publish_task(2, None, false), ClockEventAction::None);

        assert_eq!(
            event.finish_firing(),
            ClockEventAction::Program(deadline(125))
        );
        assert_eq!(event.armed_deadline(), Some(deadline(125)));
    }

    #[test]
    fn early_irq_reprograms_the_unchanged_deadline_once() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(Some(deadline(500))),
            ClockEventAction::Program(deadline(500))
        );
        assert_eq!(
            event.publish_task(1, Some(100), false),
            ClockEventAction::Program(deadline(100))
        );

        event.begin_firing();
        assert!(!event.advance_periodic(50, 25));
        assert_eq!(
            event.publish_task(2, Some(100), false),
            ClockEventAction::None,
            "updates observed while Firing must be merged into the IRQ transaction"
        );

        assert_eq!(
            event.finish_firing(),
            ClockEventAction::Program(deadline(100))
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
        assert_eq!(event.armed_deadline(), Some(deadline(100)));
    }

    #[test]
    fn spurious_irq_while_idle_is_a_bounded_noop() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(event.online(None), ClockEventAction::None);

        event.begin_firing();

        assert_eq!(event.finish_firing(), ClockEventAction::None);
        assert_eq!(event.phase(), ClockEventPhase::Idle);
        assert_eq!(event.armed_deadline(), None);
    }

    #[test]
    fn overdue_task_deadline_remains_immediate_until_firing_reconciles_it() {
        let mut event = LocalClockEvent::offline();
        event.online(Some(deadline(500)));
        assert_eq!(
            event.publish_task(1, Some(90), false),
            ClockEventAction::Program(deadline(90))
        );
        assert!(event.has_immediate_work(100));

        event.begin_firing();
        assert_eq!(event.publish_task(2, None, false), ClockEventAction::None);
        assert_eq!(
            event.finish_firing(),
            ClockEventAction::Program(deadline(500))
        );
        assert!(!event.has_immediate_work(100));
    }

    #[test]
    fn idle_recovery_claims_an_overdue_armed_event_exactly_once() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(Some(deadline(100))),
            ClockEventAction::Program(deadline(100))
        );

        assert!(event.begin_firing_if_due(100));
        assert_eq!(event.phase(), ClockEventPhase::Firing);
        assert!(!event.begin_firing_if_due(100));
    }

    #[test]
    fn abandoned_firing_transaction_can_be_recovered_and_rearmed() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(Some(deadline(500))),
            ClockEventAction::Program(deadline(500))
        );
        event.begin_firing();
        assert_eq!(
            event.publish_task(1, Some(250), true),
            ClockEventAction::None
        );

        assert_eq!(
            event.recover_firing(),
            ClockEventAction::Program(deadline(250))
        );
        assert_eq!(event.phase(), ClockEventPhase::Armed);
        assert_eq!(event.armed_deadline(), Some(deadline(250)));
        assert!(event.deferred_work());
    }
}
