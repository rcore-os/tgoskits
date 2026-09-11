//! Completion under the owning scheduler transaction.

use super::*;

impl KernelTimerQueue {
    pub(crate) fn complete_soft_execution(
        &mut self,
        mut execution: KernelTimerExecution,
        action: KernelTimerAction,
    ) -> Option<KernelTimerEntry> {
        assert!(!execution.is_hard());
        let position = self
            .executing
            .iter()
            .position(|entry| entry.identity == execution.entry.identity())
            .expect("completed kernel timer must remain in executing state");
        let executing = self.executing.swap_remove(position);
        if executing.disposition == ExecutingKernelTimerDisposition::Continue
            && let KernelTimerAction::Rearm(deadline) = action
        {
            execution.entry.rearm(deadline);
            self.insert_at(execution.entry);
            return None;
        }
        Some(execution.entry)
    }

    /// Completes one hard callback without dropping its payload in hard IRQ.
    ///
    /// Returns `true` when task-context reclamation was queued.
    pub(crate) fn complete_hard_execution(
        &mut self,
        mut execution: KernelTimerExecution,
        action: HardKernelTimerAction,
    ) -> bool {
        assert!(execution.is_hard());
        let position = self
            .executing
            .iter()
            .position(|entry| entry.identity == execution.entry.identity())
            .expect("completed hard kernel timer must remain in executing state");
        let executing = self.executing.swap_remove(position);
        match (executing.disposition, action) {
            (ExecutingKernelTimerDisposition::Destroy, _) => {
                self.completed.push(execution.entry);
                true
            }
            (ExecutingKernelTimerDisposition::Disarm, _) => {
                execution.entry.disarm();
                self.inactive.push(execution.entry);
                false
            }
            (ExecutingKernelTimerDisposition::Rearm(deadline), _) => {
                execution.entry.rearm(deadline);
                self.insert_at(execution.entry);
                false
            }
            (ExecutingKernelTimerDisposition::Continue, HardKernelTimerAction::Complete) => {
                self.completed.push(execution.entry);
                true
            }
            (ExecutingKernelTimerDisposition::Continue, HardKernelTimerAction::Disarm) => {
                execution.entry.disarm();
                self.inactive.push(execution.entry);
                false
            }
            (ExecutingKernelTimerDisposition::Continue, HardKernelTimerAction::Rearm(deadline)) => {
                execution.entry.rearm(deadline);
                self.insert_at(execution.entry);
                false
            }
        }
    }

    pub(crate) fn claim_completed(&mut self) -> Option<KernelTimerEntry> {
        (!self.completed.is_empty()).then(|| self.completed.remove(0))
    }
}
