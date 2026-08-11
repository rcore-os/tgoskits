//! Operating-system capability boundary owned by the scheduler runtime.
//!
//! Runtime resources, clock-domain values, and provider operations are split
//! by owned invariant while retaining one trait-FFI table at the OS boundary.
mod capability;
mod clock;
mod interface;

pub use capability::*;
pub use clock::*;
pub use interface::*;

pub(crate) fn enter_preempt_guard() -> PreemptGuardToken {
    let token = task_runtime::preempt_guard_enter();
    #[cfg(feature = "qperf-metrics")]
    crate::metrics::record_runtime_preempt_guard_entry(token.is_none());
    token
}

pub(crate) fn enter_irq_guard() -> IrqGuardToken {
    let token = task_runtime::irq_guard_enter();
    #[cfg(feature = "qperf-metrics")]
    crate::metrics::record_runtime_irq_guard_entry(token.is_none());
    token
}
