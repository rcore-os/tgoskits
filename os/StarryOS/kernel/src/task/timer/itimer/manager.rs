/// Process-wide task-context interval timers.
pub struct ProcessTimerManager {
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
            itimers: Default::default(),
        }
    }

    pub(crate) fn active_mask(&self) -> u8 {
        self.itimers
            .iter()
            .enumerate()
            .fold(0, |mask, (index, timer)| {
                mask | u8::from(timer.deadline_ns.is_some()) << index
            })
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
        let mut pending = PendingTimerActions::new();
        for ty in [ITimerType::Virtual, ITimerType::Prof, ITimerType::Real] {
            pending.record(ty, self.update_itimer(ty, snapshot, triggered_slot));
        }
        pending
    }

    pub(crate) fn cancel_alarms(&mut self) -> [AlarmChange; 3] {
        core::array::from_fn(|index| {
            let timer = &mut self.itimers[index];
            timer.deadline_ns = None;
            timer.alarm_slot.replace(None)
        })
    }

    /// Sets the interval timer of the specified type with the given interval
    /// and remaining time.
    pub(crate) fn set_itimer(
        &mut self,
        ty: ITimerType,
        setting: ITimerSetting,
        snapshot: ProcessCpuTimeSnapshot,
    ) -> SetITimerOutcome {
        let now_ns = ty.clock_now_ns(snapshot);
        let timer = &mut self.itimers[ty as usize];
        let old_interval = timer.interval_ns;
        let old_remaining = timer.remaining_ns(now_ns);
        SetITimerOutcome {
            old_interval: time_value_from_nanos(old_interval),
            old_remaining: time_value_from_nanos(old_remaining),
            alarm_change: timer.replace(ty, setting, now_ns),
        }
    }

    /// Gets the current interval and remaining time.
    pub fn get_itimer(
        &self,
        ty: ITimerType,
        snapshot: ProcessCpuTimeSnapshot,
    ) -> (TimeValue, TimeValue) {
        let itimer = &self.itimers[ty as usize];
        (
            time_value_from_nanos(itimer.interval_ns),
            time_value_from_nanos(itimer.remaining_ns(ty.clock_now_ns(snapshot))),
        )
    }

    fn update_itimer(
        &mut self,
        ty: ITimerType,
        snapshot: ProcessCpuTimeSnapshot,
        triggered_slot: Option<u64>,
    ) -> ITimerUpdate {
        let timer = &mut self.itimers[ty as usize];
        timer.update(
            ty,
            ty.clock_now_ns(snapshot),
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
