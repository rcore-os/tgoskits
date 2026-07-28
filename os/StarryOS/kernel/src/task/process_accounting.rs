//! Process CPU accounting and process-owned timer tables.

use alloc::sync::Arc;

use ax_runtime::hal::time::TimeValue;
use ax_sync::{PiMutex, spin::SpinNoIrq};

use super::{
    CpuTimeDelta, PosixTimerTable, ProcessCpuTimeAccounting, ProcessCpuTimeSnapshot, ProcessData,
    ProcessTimerManager, get_task,
};

/// Accounting state and timer tables shared by a thread group.
pub(super) struct ProcessAccountingState {
    children_cpu_time: SpinNoIrq<(TimeValue, TimeValue)>,
    process_cpu_time: ProcessCpuTimeAccounting,
    interval_timers: PiMutex<ProcessTimerManager>,
    posix_timers: Arc<PosixTimerTable>,
}

impl ProcessAccountingState {
    pub(super) fn new() -> Self {
        Self {
            children_cpu_time: SpinNoIrq::new((TimeValue::ZERO, TimeValue::ZERO)),
            process_cpu_time: ProcessCpuTimeAccounting::new(),
            interval_timers: PiMutex::new(ProcessTimerManager::new()),
            posix_timers: Arc::new(PosixTimerTable::default()),
        }
    }
}

impl ProcessData {
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

    /// Returns process-wide user and system CPU time.
    pub fn cpu_time(&self) -> (TimeValue, TimeValue) {
        self.cpu_time_snapshot().output()
    }

    pub fn interval_timers(&self) -> &PiMutex<ProcessTimerManager> {
        &self.accounting.interval_timers
    }

    pub fn posix_timers(&self) -> &PosixTimerTable {
        &self.accounting.posix_timers
    }
}
