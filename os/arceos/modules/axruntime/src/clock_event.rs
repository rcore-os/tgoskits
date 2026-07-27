/// Absolute non-zero deadline accepted by the physical clockevent.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct ClockDeadline(u64);

impl ClockDeadline {
    pub(crate) const fn from_nanos(deadline_ns: u64) -> Option<Self> {
        if deadline_ns == 0 {
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

    pub(crate) fn online(&mut self, periodic: ClockDeadline) -> ClockEventAction {
        assert_eq!(
            self.phase,
            ClockEventPhase::Offline,
            "clockevent may become online only once"
        );
        self.periodic_deadline = Some(periodic);
        self.phase = ClockEventPhase::Idle;
        self.reconcile_arm()
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

    /// Advances periodic accounting without producing a scheduling decision.
    ///
    /// A periodic clockevent is only one physical wakeup source. Whether the
    /// current thread must be preempted remains an ax-task policy decision.
    pub(crate) fn advance_periodic(&mut self, now_ns: u64, interval_ns: u64) {
        let Some(current) = self.periodic_deadline else {
            return;
        };
        if now_ns < current.as_nanos() {
            return;
        }
        let next = crate::next_periodic_deadline(current.as_nanos(), now_ns, interval_ns);
        self.periodic_deadline = ClockDeadline::from_nanos(next);
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
            (ClockEventPhase::Idle, _, Some(deadline)) => {
                self.armed_deadline = Some(deadline);
                self.phase = ClockEventPhase::Armed;
                ClockEventAction::Program(deadline)
            }
            (ClockEventPhase::Armed, Some(armed), Some(deadline)) if deadline < armed => {
                self.armed_deadline = Some(deadline);
                ClockEventAction::Program(deadline)
            }
            _ => ClockEventAction::None,
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
    fn earlier_task_deadline_reprograms_but_later_or_cancel_waits_for_old_event() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(500)),
            ClockEventAction::Program(deadline(500))
        );
        assert_eq!(
            event.publish_task(1, Some(300), false),
            ClockEventAction::Program(deadline(300))
        );
        assert_eq!(
            event.publish_task(2, Some(400), false),
            ClockEventAction::None
        );
        assert_eq!(event.armed_deadline(), Some(deadline(300)));
        assert_eq!(event.publish_task(3, None, false), ClockEventAction::None);
        assert_eq!(event.armed_deadline(), Some(deadline(300)));

        event.begin_firing();
        assert_eq!(
            event.finish_firing(),
            ClockEventAction::Program(deadline(500))
        );
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
            event.online(deadline(500)),
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
        event.online(deadline(100));
        event.begin_firing();
        event.advance_periodic(100, 25);
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
            event.online(deadline(100)),
            ClockEventAction::Program(deadline(100))
        );
        assert_eq!(
            event.publish_task(1, Some(100), false),
            ClockEventAction::None
        );

        event.begin_firing();
        event.advance_periodic(100, 25);
        assert_eq!(event.publish_task(2, None, false), ClockEventAction::None);

        assert_eq!(
            event.finish_firing(),
            ClockEventAction::Program(deadline(125))
        );
        assert_eq!(event.armed_deadline(), Some(deadline(125)));
    }

    #[test]
    fn overdue_task_deadline_remains_immediate_until_firing_reconciles_it() {
        let mut event = LocalClockEvent::offline();
        event.online(deadline(500));
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
    fn abandoned_firing_transaction_can_be_recovered_and_rearmed() {
        let mut event = LocalClockEvent::offline();
        assert_eq!(
            event.online(deadline(500)),
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
