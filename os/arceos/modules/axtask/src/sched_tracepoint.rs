//! Cross-crate scheduler tracepoint hook.
//!
//! Builds that enable `tracepoint-hooks` without a matching
//! `#[ax_crate_interface::impl_interface]` implementor will fail at link.

/// `prev_state` is the [`crate::task::TaskState`] discriminant; implementors
/// decode without depending on the enum.
#[ax_crate_interface::def_interface]
pub trait SchedTracepoint {
    /// Fired from `switch_to` after the `prev == next` short-circuit and
    /// before the architectural context switch. IRQs are disabled.
    fn on_sched_switch(prev_tid: u64, next_tid: u64, prev_state: u32);
}
