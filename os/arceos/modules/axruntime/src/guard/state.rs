#[derive(Clone, Copy, Debug)]
pub(super) struct RuntimeGuardState {
    pub(super) irq: RuntimeIrqState,
    pub(super) preempt: RuntimePreemptState,
    #[cfg(feature = "task-test-hooks")]
    preempt_guard_context_resolutions: usize,
    #[cfg(feature = "task-test-hooks")]
    ordinary_preempt_exit_slow_path_entries: usize,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RuntimeIrqState {
    pub(super) depth: u32,
    pub(super) outer_irqs_enabled: bool,
}

impl RuntimeIrqState {
    pub(super) const fn new() -> Self {
        Self {
            depth: 0,
            outer_irqs_enabled: false,
        }
    }

    #[cfg(any(feature = "fs", feature = "multitask", test))]
    pub(super) const fn is_clear(self) -> bool {
        self.depth == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct RuntimePreemptState {
    pub(super) scheduler_baton: SchedulerBatonState,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SchedulerBatonState {
    /// The final pending preemption depth was converted under IRQ exclusion,
    /// but ax-task has not entered its scheduler frame yet.
    #[cfg(any(feature = "multitask", test))]
    PreemptEntry,
    #[cfg(any(feature = "multitask", test))]
    Active,
    #[cfg(any(feature = "multitask", test))]
    Transferred,
    Finished,
}

impl RuntimePreemptState {
    pub(super) const fn new() -> Self {
        Self {
            scheduler_baton: SchedulerBatonState::Finished,
        }
    }

    #[cfg(any(feature = "fs", feature = "multitask", test))]
    pub(super) const fn is_clear(self) -> bool {
        matches!(self.scheduler_baton, SchedulerBatonState::Finished)
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) const fn has_one_scheduler_frame(self) -> bool {
        !matches!(self.scheduler_baton, SchedulerBatonState::Finished)
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) const fn has_active_scheduler_baton(self) -> bool {
        matches!(self.scheduler_baton, SchedulerBatonState::Active)
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) const fn has_preempt_entry_baton(self) -> bool {
        matches!(self.scheduler_baton, SchedulerBatonState::PreemptEntry)
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn claim_scheduler(&mut self) -> bool {
        if !matches!(self.scheduler_baton, SchedulerBatonState::Finished) {
            return false;
        }
        self.scheduler_baton = SchedulerBatonState::Active;
        true
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn claim_preempt_entry(&mut self) -> bool {
        if !matches!(self.scheduler_baton, SchedulerBatonState::Finished) {
            return false;
        }
        self.scheduler_baton = SchedulerBatonState::PreemptEntry;
        true
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn enter_preclaimed_scheduler(&mut self) -> bool {
        if !self.has_preempt_entry_baton() {
            return false;
        }
        self.scheduler_baton = SchedulerBatonState::Active;
        true
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn transfer_scheduler_baton(&mut self) {
        assert!(
            self.has_active_scheduler_baton(),
            "scheduler baton transfer requires the active scheduler frame"
        );
        self.scheduler_baton = SchedulerBatonState::Transferred;
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn finish_scheduler_baton(&mut self) {
        assert!(
            self.has_one_scheduler_frame(),
            "scheduler baton finish requires an active or transferred frame"
        );
        self.scheduler_baton = SchedulerBatonState::Finished;
    }
}

impl RuntimeGuardState {
    pub(super) const fn new() -> Self {
        Self {
            irq: RuntimeIrqState::new(),
            preempt: RuntimePreemptState::new(),
            #[cfg(feature = "task-test-hooks")]
            preempt_guard_context_resolutions: 0,
            #[cfg(feature = "task-test-hooks")]
            ordinary_preempt_exit_slow_path_entries: 0,
        }
    }

    #[cfg(all(feature = "task-test-hooks", not(target_arch = "x86_64")))]
    pub(super) fn record_preempt_guard_context_resolution(&mut self) {
        self.preempt_guard_context_resolutions += 1;
    }

    #[cfg(feature = "task-test-hooks")]
    pub(super) fn reset_preempt_guard_context_resolutions(&mut self) {
        self.preempt_guard_context_resolutions = 0;
    }

    #[cfg(feature = "task-test-hooks")]
    pub(super) fn take_preempt_guard_context_resolutions(&mut self) -> usize {
        core::mem::take(&mut self.preempt_guard_context_resolutions)
    }

    #[cfg(feature = "task-test-hooks")]
    pub(super) fn record_ordinary_preempt_exit_slow_path(&mut self) {
        self.ordinary_preempt_exit_slow_path_entries += 1;
    }

    #[cfg(feature = "task-test-hooks")]
    pub(super) fn reset_ordinary_preempt_exit_slow_path_entries(&mut self) {
        self.ordinary_preempt_exit_slow_path_entries = 0;
    }

    #[cfg(feature = "task-test-hooks")]
    pub(super) fn take_ordinary_preempt_exit_slow_path_entries(&mut self) -> usize {
        core::mem::take(&mut self.ordinary_preempt_exit_slow_path_entries)
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn enter_irq(&mut self, outer_irqs_enabled: bool) {
        if self.irq.depth == 0 {
            self.irq.outer_irqs_enabled = outer_irqs_enabled;
        }
        self.irq.depth = self
            .irq
            .depth
            .checked_add(1)
            .expect("runtime IRQ guard nesting overflow");
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn exit_irq(&mut self, owner: &'static str) -> bool {
        assert!(
            self.irq.depth > 0,
            "unbalanced runtime IRQ guard exit from {owner}"
        );
        self.irq.depth -= 1;
        let restore_irqs = self.irq.depth == 0 && self.irq.outer_irqs_enabled;
        if self.irq.depth == 0 {
            self.irq.outer_irqs_enabled = false;
        }
        restore_irqs
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn claim_irq_exit_scheduler(&mut self, preempt_depth: u32) -> bool {
        if self.irq.depth != 1
            || !self.irq.outer_irqs_enabled
            || preempt_depth != 0
            || !self.preempt.is_clear()
        {
            return false;
        }
        self.irq = RuntimeIrqState::new();
        self.preempt.claim_scheduler()
    }

    #[cfg(any(test, all(feature = "multitask", not(feature = "host-test"))))]
    pub(super) const fn local_scheduler_work_is_self_serviced(self, preempt_depth: u32) -> bool {
        !self.irq.is_clear() && (self.irq.outer_irqs_enabled || preempt_depth != 0)
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) const fn owns_cpu_context(self) -> bool {
        !self.irq.is_clear() || self.preempt.has_one_scheduler_frame()
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn claim_task_scheduler(&mut self, preempt_depth: u32) -> bool {
        self.irq.is_clear() && preempt_depth == 0 && self.preempt.claim_scheduler()
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn claim_preempt_exit_scheduler(&mut self, preempt_depth: u32) -> bool {
        self.irq.is_clear() && preempt_depth == 1 && self.preempt.claim_preempt_entry()
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn enter_preclaimed_scheduler(&mut self, preempt_depth: u32) -> bool {
        self.irq.is_clear() && preempt_depth == 0 && self.preempt.enter_preclaimed_scheduler()
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn exit_scheduler_preempt(&mut self, owner: &'static str) {
        assert!(
            self.irq.is_clear(),
            "{owner} exited with live IRQ guard depth={}, outer_enabled={}",
            self.irq.depth,
            self.irq.outer_irqs_enabled,
        );
        assert!(
            self.preempt.has_one_scheduler_frame(),
            "scheduler frame exit requires the exact scheduler-owned baton"
        );
        self.preempt.finish_scheduler_baton();
    }

    #[cfg(any(feature = "multitask", test))]
    pub(super) fn transfer_scheduler_preempt(&mut self) {
        assert!(
            self.irq.is_clear(),
            "scheduler baton transferred with a live IRQ guard"
        );
        self.preempt.transfer_scheduler_baton();
    }

    #[cfg(feature = "fs")]
    pub(super) const fn has_context_guard(self, preempt_depth: u32) -> bool {
        !self.irq.is_clear() || preempt_depth != 0 || !self.preempt.is_clear()
    }
}
