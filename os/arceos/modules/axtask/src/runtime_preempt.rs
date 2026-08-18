//! Narrow runtime capability used by legacy ArceOS task guards.
//!
//! The task layer owns scheduling policy and pending-work decisions. The
//! runtime owns architecture preemption state, IRQ exclusion, and the frame
//! that admits and tracks a scheduling safe point.

#[ax_crate_interface::def_interface]
pub trait RuntimePreemption {
    /// Enters architecture preemption exclusion and returns a linear token.
    fn enter() -> usize;

    /// Finishes the exact token returned by [`Self::enter`].
    fn exit(token: usize);

    /// Finishes a token at the final IRQ-return boundary.
    fn exit_from_irq_return(token: usize);

    /// Returns the architecture-selected preemption depth.
    fn depth() -> usize;

    /// Claims the CPU-local scheduler frame before selecting the next task.
    fn enter_scheduler_frame();

    /// Transfers the active scheduler frame to the raw switch continuation.
    fn transfer_scheduler_frame();

    /// Finishes the scheduler frame after the switch path returns.
    fn finish_scheduler_frame(result: SchedulerFrameResult);

    /// Completes the preemption handoff for a context's first switch-in.
    fn finish_initial_context_switch();
}

/// Describes how a scheduler frame returned to its caller.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerFrameResult {
    /// The scheduler retained the current task and did not switch contexts.
    Stayed,
    /// A raw context switch resumed an existing scheduler continuation.
    Resumed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SchedulerFrameOwnership {
    Active,
    Transferred,
}

/// Linear task-layer ownership of one CPU-local scheduler frame.
///
/// A raw switch suspends this value with the outgoing scheduler stack. When
/// that stack later resumes, the value completes the transferred frame left by
/// the switch that resumed it. A first-entry task has no suspended value and
/// consumes the transferred frame through `finish_initial_context_switch`.
pub(crate) struct SchedulerFrame {
    ownership: SchedulerFrameOwnership,
}

impl SchedulerFrame {
    pub(crate) fn enter() -> Self {
        enter_scheduler_frame();
        Self {
            ownership: SchedulerFrameOwnership::Active,
        }
    }

    pub(crate) fn transfer(&mut self) {
        assert_eq!(
            self.ownership,
            SchedulerFrameOwnership::Active,
            "scheduler frame can be transferred only once"
        );
        transfer_scheduler_frame();
        self.ownership = SchedulerFrameOwnership::Transferred;
    }

    pub(crate) fn finish(self, result: SchedulerFrameResult) {
        let expected = match result {
            SchedulerFrameResult::Stayed => SchedulerFrameOwnership::Active,
            SchedulerFrameResult::Resumed => SchedulerFrameOwnership::Transferred,
        };
        assert_eq!(
            self.ownership, expected,
            "scheduler return does not match task-layer frame ownership"
        );
        finish_scheduler_frame(result);
    }
}

#[inline(always)]
#[cfg(feature = "preempt")]
pub(crate) fn enter() -> usize {
    ax_crate_interface::call_interface!(RuntimePreemption::enter)
}

#[inline(always)]
#[cfg(feature = "preempt")]
pub(crate) fn exit(token: usize) {
    ax_crate_interface::call_interface!(RuntimePreemption::exit, token)
}

#[inline(always)]
#[cfg(feature = "preempt")]
pub(crate) fn exit_from_irq_return(token: usize) {
    ax_crate_interface::call_interface!(RuntimePreemption::exit_from_irq_return, token)
}

#[inline(always)]
#[cfg(all(feature = "preempt", not(feature = "host-test")))]
pub(crate) fn depth() -> usize {
    ax_crate_interface::call_interface!(RuntimePreemption::depth)
}

#[inline(always)]
pub(crate) fn enter_scheduler_frame() {
    #[cfg(all(feature = "preempt", not(feature = "host-test")))]
    ax_crate_interface::call_interface!(RuntimePreemption::enter_scheduler_frame);
}

#[inline(always)]
pub(crate) fn transfer_scheduler_frame() {
    #[cfg(all(feature = "preempt", not(feature = "host-test")))]
    ax_crate_interface::call_interface!(RuntimePreemption::transfer_scheduler_frame);
}

#[inline(always)]
pub(crate) fn finish_scheduler_frame(result: SchedulerFrameResult) {
    #[cfg(all(feature = "preempt", not(feature = "host-test")))]
    ax_crate_interface::call_interface!(RuntimePreemption::finish_scheduler_frame, result);
    #[cfg(any(not(feature = "preempt"), feature = "host-test"))]
    let _ = result;
}

#[inline(always)]
pub(crate) fn finish_initial_context_switch() {
    #[cfg(all(feature = "preempt", not(feature = "host-test")))]
    ax_crate_interface::call_interface!(RuntimePreemption::finish_initial_context_switch);
    #[cfg(all(feature = "preempt", feature = "host-test"))]
    crate::sync::finish_initial_host_context_switch();
}
