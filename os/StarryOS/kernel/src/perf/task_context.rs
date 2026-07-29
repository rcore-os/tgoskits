//! Per-thread perf admission and scheduler-list ownership.
//!
//! Linux serializes `perf_event_open()` against `perf_event_exit_task()` with
//! the task perf mutex and a tombstoned context. This object is the equivalent
//! Starry boundary: attach either commits before the exit snapshot or observes
//! the tombstone and returns `ESRCH`.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_errno::{AxError, AxResult};
use ax_kspin::SpinNoIrq;

use super::{
    task::PerTaskCounter,
    task_context_state::{PerfAttachError, PerfTaskContextState},
};

const PERF_COUNTER_CAPACITY: usize = 32;

/// Number of task counters with a committed scheduler-list reservation.
pub(super) static PERF_TASK_ACTIVE: AtomicUsize = AtomicUsize::new(0);

/// One task's fixed, IRQ-safe scheduler list and exit admission state.
pub(crate) struct ThreadPerfContext {
    state: SpinNoIrq<PerfTaskContextState<Arc<PerTaskCounter>, PERF_COUNTER_CAPACITY>>,
}

impl ThreadPerfContext {
    /// Creates an empty context that accepts event installation.
    pub(crate) const fn new() -> Self {
        Self {
            state: SpinNoIrq::new(PerfTaskContextState::new()),
        }
    }

    /// Commits one scheduler-visible counter or rejects a tombstoned task.
    pub(crate) fn attach(&self, counter: Arc<PerTaskCounter>) -> AxResult<()> {
        let mut state = self.state.lock();
        // Closed events may remain family-owned for aggregate reads. Reclaim
        // their list slots in task context before admitting a new live event.
        state.retain(|counter| !counter.resources_released());
        state
            .attach(Arc::clone(&counter))
            .map_err(|error| match error {
                PerfAttachError::Closed => AxError::NoSuchProcess,
                PerfAttachError::Full => AxError::NoMemory,
            })?;
        assert!(
            counter.publish_scheduler_registration(),
            "only a reserved PMU counter may enter a task perf context"
        );
        // Publish the global fast-path key while the list lock is still held.
        // A scheduler that observes the increment then either sees the entry or
        // waits for this bounded publication transaction to finish.
        PERF_TASK_ACTIVE.fetch_add(1, Ordering::AcqRel);
        Ok(())
    }

    /// Withdraws an attached counter whose wider publication failed.
    pub(crate) fn detach_unpublished(&self, counter: &Arc<PerTaskCounter>) {
        let removed = self
            .state
            .lock()
            .remove(|candidate| Arc::ptr_eq(candidate, counter));
        // A later attach may reclaim this list slot after `free_hw` completes
        // but before the failed opener reaches its final detach. Missing is
        // therefore valid only after the exact PMU resource was quiesced.
        assert!(
            removed || counter.resources_released(),
            "a live unpublished perf counter lost its local reservation"
        );
    }

    /// Snapshots counters for task-context sideband/control work.
    pub(crate) fn snapshot(&self) -> heapless::Vec<Arc<PerTaskCounter>, PERF_COUNTER_CAPACITY> {
        self.state.lock().snapshot()
    }

    /// Snapshots a live parent context for pre-publication inheritance.
    pub(crate) fn snapshot_for_inherit(
        &self,
    ) -> Option<heapless::Vec<Arc<PerTaskCounter>, PERF_COUNTER_CAPACITY>> {
        self.state.lock().snapshot_if_accepting()
    }

    /// Permanently rejects later opens and returns the complete exit snapshot.
    pub(crate) fn close_and_snapshot(
        &self,
    ) -> heapless::Vec<Arc<PerTaskCounter>, PERF_COUNTER_CAPACITY> {
        self.state.lock().close_snapshot()
    }

    /// Runs one bounded scheduler hook while retaining list stability.
    pub(crate) fn with_counters<R>(
        &self,
        operation: impl FnOnce(&[Arc<PerTaskCounter>]) -> R,
    ) -> R {
        let state = self.state.lock();
        operation(state.counters())
    }
}
