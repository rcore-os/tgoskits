#[derive(Clone, Copy, Debug)]
pub(super) struct RuntimeGuardState {
    pub(super) irq: RuntimeIrqState,
    pub(super) preempt: RuntimePreemptState,
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
    PreemptEntry,
    Active,
    Transferred,
    Finished,
}

impl RuntimePreemptState {
    pub(super) const fn new() -> Self {
        Self {
            scheduler_baton: SchedulerBatonState::Finished,
        }
    }

    pub(super) const fn is_clear(self) -> bool {
        matches!(self.scheduler_baton, SchedulerBatonState::Finished)
    }
    pub(super) const fn has_one_scheduler_frame(self) -> bool {
        !matches!(self.scheduler_baton, SchedulerBatonState::Finished)
    }
    pub(super) const fn has_active_scheduler_baton(self) -> bool {
        matches!(self.scheduler_baton, SchedulerBatonState::Active)
    }
    pub(super) const fn has_preempt_entry_baton(self) -> bool {
        matches!(self.scheduler_baton, SchedulerBatonState::PreemptEntry)
    }
    pub(super) fn claim_scheduler(&mut self) -> bool {
        if !matches!(self.scheduler_baton, SchedulerBatonState::Finished) {
            return false;
        }
        self.scheduler_baton = SchedulerBatonState::Active;
        true
    }
    pub(super) fn claim_preempt_entry(&mut self) -> bool {
        if !matches!(self.scheduler_baton, SchedulerBatonState::Finished) {
            return false;
        }
        self.scheduler_baton = SchedulerBatonState::PreemptEntry;
        true
    }
    pub(super) fn enter_preclaimed_scheduler(&mut self) -> bool {
        if !self.has_preempt_entry_baton() {
            return false;
        }
        self.scheduler_baton = SchedulerBatonState::Active;
        true
    }
    pub(super) fn commit_prepared_scheduler_baton(&mut self) {
        debug_assert!(
            self.has_active_scheduler_baton(),
            "prepared scheduler baton changed before transfer"
        );
        self.scheduler_baton = SchedulerBatonState::Transferred;
    }
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
        }
    }
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

    #[cfg(any(test, not(feature = "host-test")))]
    pub(super) const fn local_scheduler_work_is_self_serviced(self, preempt_depth: u32) -> bool {
        !self.irq.is_clear() && (self.irq.outer_irqs_enabled || preempt_depth != 0)
    }
    pub(super) const fn owns_cpu_context(self) -> bool {
        !self.irq.is_clear() || self.preempt.has_one_scheduler_frame()
    }
    pub(super) fn claim_task_scheduler(&mut self, preempt_depth: u32) -> bool {
        self.irq.is_clear() && preempt_depth == 0 && self.preempt.claim_scheduler()
    }
    pub(super) fn claim_preempt_exit_scheduler(&mut self, preempt_depth: u32) -> bool {
        self.irq.is_clear() && preempt_depth == 1 && self.preempt.claim_preempt_entry()
    }
    pub(super) fn enter_preclaimed_scheduler(&mut self, preempt_depth: u32) -> bool {
        self.irq.is_clear() && preempt_depth == 0 && self.preempt.enter_preclaimed_scheduler()
    }
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
    pub(super) fn commit_prepared_scheduler_preempt(&mut self) {
        debug_assert!(
            self.irq.is_clear(),
            "prepared scheduler baton gained a live IRQ guard before transfer"
        );
        self.preempt.commit_prepared_scheduler_baton();
    }

    #[cfg(feature = "fs")]
    pub(super) const fn has_context_guard(self, preempt_depth: u32) -> bool {
        !self.irq.is_clear() || preempt_depth != 0 || !self.preempt.is_clear()
    }
}
