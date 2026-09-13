//! Runtime observations for scheduler and execution-context diagnostics.

#[cfg(feature = "qperf-metrics")]
pub use crate::thread::scheduler_events::{
    QperfRuntimeSchedulerMetricsSnapshot, qperf_runtime_scheduler_metrics_snapshot,
};
pub use crate::thread::{
    context::diagnose_current_stack_guard_page_fault,
    runtime_impl::{
        SchedSwitchTraceHook, install_sched_switch_trace_hook, publish_sched_switch_trace_gate,
    },
    scheduler_events::timer_irq_count,
};
