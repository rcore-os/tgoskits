//! Narrow runtime capability used by legacy ArceOS task guards.
//!
//! The task layer owns scheduling policy and pending-work decisions. The
//! runtime owns architecture preemption state, IRQ exclusion, and the baton
//! that admits a scheduling safe point.

#[ax_crate_interface::def_interface]
pub trait RuntimePreemption {
    /// Enters architecture preemption exclusion and returns a linear token.
    fn enter() -> usize;

    /// Finishes the exact token returned by [`Self::enter`].
    fn exit(token: usize);

    /// Returns the architecture-selected preemption depth.
    fn depth() -> usize;

    /// Completes the preemption handoff for a context's first switch-in.
    fn finish_initial_context_switch();
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
#[cfg(all(feature = "preempt", not(feature = "host-test")))]
pub(crate) fn depth() -> usize {
    ax_crate_interface::call_interface!(RuntimePreemption::depth)
}

#[inline(always)]
pub(crate) fn finish_initial_context_switch() {
    #[cfg(all(feature = "preempt", not(feature = "host-test")))]
    ax_crate_interface::call_interface!(RuntimePreemption::finish_initial_context_switch);
    #[cfg(all(feature = "preempt", feature = "host-test"))]
    crate::sync::finish_initial_host_context_switch();
}
