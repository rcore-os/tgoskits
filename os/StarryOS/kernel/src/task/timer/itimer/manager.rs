/// Process-wide task-context interval timers.
pub struct ProcessTimerManager {
    itimers: [ITimer; 3],
    // Only ITIMER_REAL is backed by the wall-clock alarm worker.
    // ITIMER_VIRTUAL and ITIMER_PROF advance at scheduler accounting/resume
    // safe points, matching Linux CPU-timer semantics and avoiding idle polling.
    real_alarm_slot: AlarmSlot,
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
            real_alarm_slot: AlarmSlot::new(),
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

    /// Polls interval timers at an accounting or task-resume safe point.
    ///
    /// The returned actions are applied after releasing timer metadata, so
    /// signal delivery and wall-alarm publication cannot re-enter this manager.
    pub(crate) fn poll(&mut self, snapshot: ProcessCpuTimeSnapshot) -> PendingTimerActions {
        self.poll_at(snapshot, false)
    }

    pub(crate) fn poll_for_alarm(
        &mut self,
        snapshot: ProcessCpuTimeSnapshot,
        token: &AlarmToken,
    ) -> PendingTimerActions {
        if !self.real_alarm_slot.matches(token) {
            return PendingTimerActions::new();
        }
        self.poll_at(snapshot, true)
    }

    fn poll_at(
        &mut self,
        snapshot: ProcessCpuTimeSnapshot,
        real_alarm_triggered: bool,
    ) -> PendingTimerActions {
        let mut pending = PendingTimerActions::new();
        for ty in [ITimerType::Virtual, ITimerType::Prof, ITimerType::Real] {
            pending.record(
                ty,
                self.update_itimer(ty, snapshot, real_alarm_triggered),
            );
        }
        pending
    }

    pub(crate) fn cancel_alarm(&mut self) -> AlarmChange {
        for timer in &mut self.itimers {
            timer.deadline_ns = None;
        }
        self.real_alarm_slot.replace(None)
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
        let (old_interval, old_remaining) = {
            let timer = &mut self.itimers[ty as usize];
            let old_interval = timer.interval_ns;
            let old_remaining = timer.remaining_ns(now_ns);
            timer.replace(setting, now_ns);
            (old_interval, old_remaining)
        };
        SetITimerOutcome {
            old_interval: time_value_from_nanos(old_interval),
            old_remaining: time_value_from_nanos(old_remaining),
            alarm_change: (ty == ITimerType::Real).then(|| self.replace_real_alarm(now_ns)),
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
        real_alarm_triggered: bool,
    ) -> ITimerUpdate {
        let now_ns = ty.clock_now_ns(snapshot);
        let expired = self.itimers[ty as usize].update(now_ns);
        let alarm_change = (ty == ITimerType::Real && (expired || real_alarm_triggered))
            .then(|| self.replace_real_alarm(now_ns));
        ITimerUpdate {
            expired,
            alarm_change,
        }
    }

    fn replace_real_alarm(&self, now_ns: u64) -> AlarmChange {
        let deadline_ns = self.itimers[ITimerType::Real as usize].deadline_ns;
        self.real_alarm_slot.replace(deadline_ns.map(|deadline_ns| {
            real_itimer_alarm_delay(deadline_ns.saturating_sub(now_ns))
        }))
    }
}

/// Result of replacing one interval timer while its metadata is locked.
pub(crate) struct SetITimerOutcome {
    old_interval: TimeValue,
    old_remaining: TimeValue,
    alarm_change: Option<AlarmChange>,
}

impl SetITimerOutcome {
    pub(crate) fn apply(self, target: AlarmTarget) -> (TimeValue, TimeValue) {
        if let Some(alarm_change) = self.alarm_change {
            alarm_change.apply(target);
        }
        (self.old_interval, self.old_remaining)
    }

    #[cfg(any(test, axtest))]
    pub(super) const fn publishes_wall_alarm(&self) -> bool {
        self.alarm_change.is_some()
    }
}

/// Fixed-size task-context actions returned after releasing timer metadata.
#[derive(Default)]
pub(crate) struct PendingTimerActions {
    signals: [Option<Signo>; 3],
    alarm_change: Option<AlarmChange>,
}

impl PendingTimerActions {
    const fn new() -> Self {
        Self {
            signals: [None; 3],
            alarm_change: None,
        }
    }

    fn record(&mut self, timer: ITimerType, update: ITimerUpdate) {
        if update.expired {
            self.signals[timer as usize] = Some(timer.signo());
        }
        if update.alarm_change.is_some() {
            debug_assert!(self.alarm_change.is_none());
            self.alarm_change = update.alarm_change;
        }
    }

    pub(crate) fn signals(&self) -> impl Iterator<Item = Signo> + '_ {
        self.signals.into_iter().flatten()
    }

    pub(crate) fn apply_alarms(self, target: AlarmTarget) {
        if let Some(alarm_change) = self.alarm_change {
            alarm_change.apply(target);
        }
    }

    #[cfg(any(test, axtest))]
    pub(super) fn publishes_wall_alarm(&self) -> bool {
        self.alarm_change.is_some()
    }
}
