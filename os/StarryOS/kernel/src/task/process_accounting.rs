//! Process CPU accounting and process-owned timer tables.

use alloc::sync::Arc;
#[cfg(target_arch = "aarch64")]
use alloc::sync::Weak;
#[cfg(target_arch = "aarch64")]
use core::sync::atomic::AtomicUsize;
use core::sync::atomic::{AtomicU8, Ordering};

use ax_runtime::{hal::time::TimeValue, task::runtime::service::SchedulerTickGate};
use linux_raw_sys::general::RLIMIT_RTTIME;

use super::{
    AlarmChange, AlarmToken, CpuTimeDelta, ITimerSetting, ITimerType, PendingTimerActions,
    PosixTimerTable, ProcessCpuTimeAccounting, ProcessCpuTimeSnapshot, ProcessData,
    ProcessTimerManager, SetITimerOutcome, get_task_by_number,
};
use crate::sync::{IrqMutex, Mutex};

const CPU_INTERVAL_TIMER_MASK: u8 =
    (1 << ITimerType::Virtual as usize) | (1 << ITimerType::Prof as usize);

/// Accounting state and timer tables shared by a thread group.
pub(super) struct ProcessAccountingState {
    children_cpu_time: IrqMutex<(TimeValue, TimeValue)>,
    process_cpu_time: ProcessCpuTimeAccounting,
    interval_timers: Mutex<ProcessTimerManager>,
    active_interval_timers: AtomicU8,
    #[cfg(target_arch = "aarch64")]
    perf_scheduler_tick_users: AtomicUsize,
    scheduler_tick_gate: Arc<SchedulerTickGate>,
    /// Serializes source observation with scheduler gate publication.
    scheduler_tick_publish: IrqMutex<()>,
    posix_timers: Arc<PosixTimerTable>,
}

impl ProcessAccountingState {
    pub(super) fn new() -> Self {
        Self {
            children_cpu_time: IrqMutex::new((TimeValue::ZERO, TimeValue::ZERO)),
            process_cpu_time: ProcessCpuTimeAccounting::new(),
            interval_timers: Mutex::new(ProcessTimerManager::new()),
            active_interval_timers: AtomicU8::new(0),
            #[cfg(target_arch = "aarch64")]
            perf_scheduler_tick_users: AtomicUsize::new(0),
            scheduler_tick_gate: Arc::new(SchedulerTickGate::new()),
            scheduler_tick_publish: IrqMutex::new(()),
            posix_timers: Arc::new(PosixTimerTable::default()),
        }
    }
}

impl ProcessData {
    fn refresh_scheduler_tick_gate(&self) {
        self.accounting.publish_scheduler_tick_gate(
            || {
                let has_cpu_interval_timer = self
                    .accounting
                    .active_interval_timers
                    .load(Ordering::Acquire)
                    & CPU_INTERVAL_TIMER_MASK
                    != 0;
                let has_rttime_watchdog = self.rlimit_current(RLIMIT_RTTIME) != u64::MAX;
                let enabled = has_cpu_interval_timer || has_rttime_watchdog;
                #[cfg(target_arch = "aarch64")]
                let enabled = enabled
                    || self
                        .accounting
                        .perf_scheduler_tick_users
                        .load(Ordering::Acquire)
                        != 0;
                enabled
            },
            || {},
        );
    }

    fn publish_active_interval_timers(&self, mask: u8) {
        self.accounting
            .active_interval_timers
            .store(mask, Ordering::Release);
        self.refresh_scheduler_tick_gate();
    }

    pub(crate) fn publish_rttime_watchdog_limit(&self) {
        self.refresh_scheduler_tick_gate();
    }

    pub(crate) fn scheduler_tick_gate(&self) -> Arc<SchedulerTickGate> {
        Arc::clone(&self.accounting.scheduler_tick_gate)
    }

    #[cfg(target_arch = "aarch64")]
    pub(crate) fn acquire_perf_scheduler_tick(self: &Arc<Self>) -> PerfSchedulerTickLease {
        self.accounting
            .perf_scheduler_tick_users
            .fetch_add(1, Ordering::AcqRel);
        self.refresh_scheduler_tick_gate();
        PerfSchedulerTickLease {
            process: Arc::downgrade(self),
        }
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
            .snapshot_with_live(|_now_ns| {
                self.proc
                    .threads()
                    .into_iter()
                    .filter_map(|tid| get_task_by_number(tid).ok())
                    .fold(CpuTimeDelta::ZERO, |total, task| {
                        let thread = task.as_thread();
                        total.add(
                            thread
                                .cpu_time()
                                .unpublished_delta(thread.scheduler_runtime_ns()),
                        )
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

impl ProcessAccountingState {
    fn publish_scheduler_tick_gate(
        &self,
        sources_enabled: impl FnOnce() -> bool,
        before_publish: impl FnOnce(),
    ) {
        // Source changes publish before calling us. Serialize the complete
        // observation/store pair so an older refresh cannot disable a gate
        // that a later interest acquisition has already enabled. This region
        // reads atomics only and never takes timer or scheduler task locks.
        let _publish = self.scheduler_tick_publish.lock();
        let enabled = sources_enabled();
        before_publish();
        self.scheduler_tick_gate.set_enabled(enabled);
    }
}

/// RAII interest keeping scheduler-tick task work enabled for PMU rotation.
#[derive(Debug)]
#[cfg(target_arch = "aarch64")]
pub(crate) struct PerfSchedulerTickLease {
    process: Weak<ProcessData>,
}

#[cfg(target_arch = "aarch64")]
impl Drop for PerfSchedulerTickLease {
    fn drop(&mut self) {
        let Some(process) = self.process.upgrade() else {
            return;
        };
        let previous = process
            .accounting
            .perf_scheduler_tick_users
            .fetch_sub(1, Ordering::AcqRel);
        assert!(previous > 0, "perf scheduler-tick interest underflow");
        process.refresh_scheduler_tick_gate();
    }
}

#[cfg(all(test, axtest))]
mod tests {
    #[axtest::axtest]
    fn tick_gate_source_snapshot_and_publication_are_one_transaction() {
        let accounting = super::ProcessAccountingState::new();
        let intervened = core::cell::Cell::new(false);
        accounting.publish_scheduler_tick_gate(
            || false,
            || {
                // A competing refresher must not publish a newer enabled state
                // between this caller's source snapshot and its disabled store.
                if let Some(_other_publisher) = accounting.scheduler_tick_publish.try_lock() {
                    accounting.scheduler_tick_gate.set_enabled(true);
                    intervened.set(true);
                }
            },
        );
        assert!(
            !intervened.get(),
            "stale tick refresh can overwrite a newer publication"
        );
    }
}
