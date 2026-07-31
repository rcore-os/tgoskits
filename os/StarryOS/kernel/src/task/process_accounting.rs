//! Process CPU accounting and process-owned timer tables.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU8, Ordering};

use ax_runtime::{hal::time::TimeValue, task::SchedulerTickGate};
use ax_sync::{PiMutex, spin::SpinNoIrq};

use super::{
    AlarmChange, AlarmToken, CpuTimeDelta, ITimerSetting, ITimerType, PendingTimerActions,
    PosixTimerTable, ProcessCpuTimeAccounting, ProcessCpuTimeSnapshot, ProcessData,
    ProcessTimerManager, SetITimerOutcome, get_task,
};

const CPU_INTERVAL_TIMER_MASK: u8 =
    (1 << ITimerType::Virtual as usize) | (1 << ITimerType::Prof as usize);

/// Accounting state and timer tables shared by a thread group.
pub(super) struct ProcessAccountingState {
    children_cpu_time: SpinNoIrq<(TimeValue, TimeValue)>,
    process_cpu_time: ProcessCpuTimeAccounting,
    interval_timers: PiMutex<ProcessTimerManager>,
    active_interval_timers: AtomicU8,
    scheduler_tick_gate: Arc<SchedulerTickGate>,
    posix_timers: Arc<PosixTimerTable>,
}

impl ProcessAccountingState {
    pub(super) fn new() -> Self {
        Self {
            children_cpu_time: SpinNoIrq::new((TimeValue::ZERO, TimeValue::ZERO)),
            process_cpu_time: ProcessCpuTimeAccounting::new(),
            interval_timers: PiMutex::new(ProcessTimerManager::new()),
            active_interval_timers: AtomicU8::new(0),
            scheduler_tick_gate: Arc::new(SchedulerTickGate::new()),
            posix_timers: Arc::new(PosixTimerTable::default()),
        }
    }
}

impl ProcessData {
    fn publish_active_interval_timers(&self, mask: u8) {
        self.accounting
            .active_interval_timers
            .store(mask, Ordering::Release);
        self.accounting
            .scheduler_tick_gate
            .set_enabled(mask & CPU_INTERVAL_TIMER_MASK != 0);
    }

    pub(crate) fn scheduler_tick_gate(&self) -> Arc<SchedulerTickGate> {
        Arc::clone(&self.accounting.scheduler_tick_gate)
    }

    pub(crate) fn record_cpu_time_transition(&self, transition: impl FnOnce() -> CpuTimeDelta) {
        self.accounting
            .process_cpu_time
            .record_transition(transition);
    }

    /// Returns accumulated CPU time of waited children.
    pub fn children_cpu_time(&self) -> (TimeValue, TimeValue) {
        *self.accounting.children_cpu_time.lock()
    }

    /// Adds a reaped child's CPU time to this process.
    pub fn add_child_cpu_time(&self, utime: TimeValue, stime: TimeValue) {
        let mut time = self.accounting.children_cpu_time.lock();
        time.0 += utime;
        time.1 += stime;
    }

    pub(crate) fn cpu_time_snapshot(&self) -> ProcessCpuTimeSnapshot {
        self.accounting
            .process_cpu_time
            .snapshot_with_live(|now_ns| {
                self.proc
                    .threads()
                    .into_iter()
                    .filter_map(|tid| get_task(tid).ok())
                    .fold(CpuTimeDelta::ZERO, |total, task| {
                        total.add(task.as_thread().cpu_time().running_residual_at(now_ns))
                    })
            })
    }

    pub(crate) fn scheduler_tick_cpu_time_snapshot(&self) -> ProcessCpuTimeSnapshot {
        self.accounting.process_cpu_time.snapshot_committed()
    }

    /// Returns process-wide user and system CPU time.
    pub fn cpu_time(&self) -> (TimeValue, TimeValue) {
        self.cpu_time_snapshot().output()
    }

    pub(crate) fn has_active_interval_timers(&self) -> bool {
        self.accounting
            .active_interval_timers
            .load(Ordering::Acquire)
            != 0
    }

    pub(crate) fn has_active_cpu_interval_timers(&self) -> bool {
        self.accounting
            .active_interval_timers
            .load(Ordering::Acquire)
            & CPU_INTERVAL_TIMER_MASK
            != 0
    }

    pub(crate) fn poll_interval_timers(
        &self,
        snapshot: ProcessCpuTimeSnapshot,
        token: Option<&AlarmToken>,
    ) -> Option<PendingTimerActions> {
        if !self.has_active_interval_timers() {
            return None;
        }

        let mut timers = self.accounting.interval_timers.lock();
        let pending = match token {
            Some(token) => timers.poll_for_alarm(snapshot, token),
            None => timers.poll(snapshot),
        };
        self.publish_active_interval_timers(timers.active_mask());
        Some(pending)
    }

    pub(crate) fn poll_cpu_interval_timers(
        &self,
        snapshot: ProcessCpuTimeSnapshot,
    ) -> Option<PendingTimerActions> {
        if !self.has_active_cpu_interval_timers() {
            return None;
        }

        let mut timers = self.accounting.interval_timers.lock();
        let pending = timers.poll_cpu(snapshot);
        self.publish_active_interval_timers(timers.active_mask());
        Some(pending)
    }

    pub fn get_interval_timer(&self, timer: ITimerType) -> (TimeValue, TimeValue) {
        let snapshot = self.cpu_time_snapshot();
        self.accounting
            .interval_timers
            .lock()
            .get_itimer(timer, snapshot)
    }

    pub(crate) fn set_interval_timer(
        &self,
        timer: ITimerType,
        interval: TimeValue,
        remaining: TimeValue,
    ) -> SetITimerOutcome {
        let setting = ITimerSetting::new(interval, remaining);
        let snapshot = self.cpu_time_snapshot();
        let mut timers = self.accounting.interval_timers.lock();
        let outcome = timers.set_itimer(timer, setting, snapshot);
        self.publish_active_interval_timers(timers.active_mask());
        outcome
    }

    pub(crate) fn cancel_interval_timer_alarm(&self) -> AlarmChange {
        let mut timers = self.accounting.interval_timers.lock();
        let cancellation = timers.cancel_alarm();
        self.publish_active_interval_timers(0);
        cancellation
    }

    pub fn posix_timers(&self) -> &PosixTimerTable {
        &self.accounting.posix_timers
    }
}
