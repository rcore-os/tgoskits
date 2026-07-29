/// Process-wide task-context interval timers.
pub struct ProcessTimerManager {
    last_wall_ns: u64,
    last_user_ns: u64,
    last_system_ns: u64,
    itimers: [ITimer; 3],
}

impl Default for ProcessTimerManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessTimerManager {
    pub(crate) fn new() -> Self {
        Self {
            last_wall_ns: 0,
            last_user_ns: 0,
            last_system_ns: 0,
            itimers: Default::default(),
        }
    }

    /// Polls CPU/wall interval timers without invoking external code.
    pub(crate) fn poll(&mut self, snapshot: ProcessCpuTimeSnapshot) -> PendingTimerActions {
        self.poll_at(snapshot, None)
    }

    pub(crate) fn poll_for_alarm(
        &mut self,
        snapshot: ProcessCpuTimeSnapshot,
        token: &AlarmToken,
    ) -> PendingTimerActions {
        let Some(slot_id) = self
            .itimers
            .iter()
            .find(|timer| timer.alarm_slot.matches(token))
            .map(|timer| timer.alarm_slot.id())
        else {
            return PendingTimerActions::new();
        };
        self.poll_at(snapshot, Some(slot_id))
    }

    fn poll_at(
        &mut self,
        snapshot: ProcessCpuTimeSnapshot,
        triggered_slot: Option<u64>,
    ) -> PendingTimerActions {
        let user_delta = snapshot.user_ns.saturating_sub(self.last_user_ns);
        let system_delta = snapshot.system_ns.saturating_sub(self.last_system_ns);
        let mut pending = PendingTimerActions::new();
        pending.record(
            ITimerType::Virtual,
            self.update_itimer(ITimerType::Virtual, timer_delta(user_delta), triggered_slot),
        );
        pending.record(
            ITimerType::Prof,
            self.update_itimer(
                ITimerType::Prof,
                timer_delta(user_delta.saturating_add(system_delta)),
                triggered_slot,
            ),
        );
        pending.record(
            ITimerType::Real,
            self.update_itimer(
                ITimerType::Real,
                timer_delta(snapshot.sampled_at_ns.saturating_sub(self.last_wall_ns)),
                triggered_slot,
            ),
        );
        self.last_user_ns = snapshot.user_ns;
        self.last_system_ns = snapshot.system_ns;
        self.last_wall_ns = snapshot.sampled_at_ns;
        pending
    }

    pub(crate) fn cancel_alarms(&mut self) -> [AlarmChange; 3] {
        core::array::from_fn(|index| {
            let timer = &mut self.itimers[index];
            timer.remained_ns = 0;
            timer.alarm_slot.replace(None)
        })
    }

    /// Sets the interval timer of the specified type with the given interval
    /// and remaining time.
    pub(crate) fn set_itimer(
        &mut self,
        ty: ITimerType,
        interval_ns: usize,
        remained_ns: usize,
    ) -> SetITimerOutcome {
        let timer = &mut self.itimers[ty as usize];
        let old_interval = timer.interval_ns;
        let old_remaining = timer.remained_ns;
        timer.interval_ns = interval_ns;
        timer.remained_ns = remained_ns;
        SetITimerOutcome {
            old_interval: time_value_from_nanos(old_interval as u64),
            old_remaining: time_value_from_nanos(old_remaining as u64),
            alarm_change: timer
                .alarm_slot
                .replace((remained_ns > 0).then(|| itimer_alarm_delay(ty, remained_ns))),
        }
    }

    /// Gets the current interval and remaining time.
    pub fn get_itimer(&self, ty: ITimerType) -> (TimeValue, TimeValue) {
        let itimer = &self.itimers[ty as usize];
        (
            time_value_from_nanos(itimer.interval_ns as u64),
            time_value_from_nanos(itimer.remained_ns as u64),
        )
    }

    fn update_itimer(
        &mut self,
        ty: ITimerType,
        delta: usize,
        triggered_slot: Option<u64>,
    ) -> ITimerUpdate {
        let timer = &mut self.itimers[ty as usize];
        timer.update(
            ty,
            delta,
            triggered_slot.is_some_and(|slot| slot == timer.alarm_slot.id()),
        )
    }
}

/// Result of replacing one interval timer while its metadata is locked.
pub(crate) struct SetITimerOutcome {
    old_interval: TimeValue,
    old_remaining: TimeValue,
    alarm_change: AlarmChange,
}

impl SetITimerOutcome {
    pub(crate) fn apply(self, target: AlarmTarget) -> (TimeValue, TimeValue) {
        self.alarm_change.apply(target);
        (self.old_interval, self.old_remaining)
    }
}

/// Fixed-size task-context actions returned after releasing timer metadata.
#[derive(Default)]
pub(crate) struct PendingTimerActions {
    signals: [Option<Signo>; 3],
    alarm_changes: [Option<AlarmChange>; 3],
}

impl PendingTimerActions {
    const fn new() -> Self {
        Self {
            signals: [None; 3],
            alarm_changes: [None, None, None],
        }
    }

    fn record(&mut self, timer: ITimerType, update: ITimerUpdate) {
        if update.expired {
            self.signals[timer as usize] = Some(timer.signo());
        }
        self.alarm_changes[timer as usize] = update.alarm_change;
    }

    pub(crate) fn signals(&self) -> impl Iterator<Item = Signo> + '_ {
        self.signals.into_iter().flatten()
    }

    pub(crate) fn apply_alarms(self, target: AlarmTarget) {
        apply_alarm_changes(self.alarm_changes.into_iter().flatten(), target);
    }
}

fn timer_delta(delta: u64) -> usize {
    delta.min(usize::MAX as u64) as usize
}

