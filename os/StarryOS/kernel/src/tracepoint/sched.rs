//! `sched:*` tracepoints.
//!
//! `sched_switch` is fired by `ax-task` through the cross-crate
//! [`ax_task::SchedTracepoint`] interface (gated by `tracepoint-hooks`).
//!
//! The other two `sched:*` events are defined next to their emission sites
//! rather than here: `sched_process_fork` in `crate::syscall::task::clone`
//! and `sched_process_exit` in `crate::task::ops`. Registration is by link
//! section, so their physical location does not affect discovery.

use ax_task::{SchedTracepoint, TaskId, task_by_id};

use crate::task::AsThread;

ktracepoint::define_event_trace!(
    sched_switch,
    TP_kops(crate::tracepoint::KernelTraceAux),
    TP_system(sched),
    TP_PROTO(prev_tid: u64, next_tid: u64, prev_state: u32),
    TP_STRUCT__entry {
        prev_tid: u64,
        next_tid: u64,
        prev_state: u32,
    },
    TP_fast_assign {
        prev_tid: prev_tid,
        next_tid: next_tid,
        prev_state: prev_state,
    },
    TP_ident(__entry),
    TP_printk({
        alloc::format!(
            "prev_tid={} next_tid={} prev_state={}",
            __entry.prev_tid,
            __entry.next_tid,
            __entry.prev_state,
        )
    })
);

struct SchedTracepointImpl;

#[ax_crate_interface::impl_interface]
impl SchedTracepoint for SchedTracepointImpl {
    fn on_sched_switch(prev_task: TaskId, next_task: TaskId, prev_state: u32) {
        trace_sched_switch(trace_tid(prev_task), trace_tid(next_task), prev_state);
    }
}

/// Project a scheduler-only task identity into the root Linux TID view.
/// Kernel tasks have no Starry PID identity, so their typed scheduler ID is
/// used only as the trace wire fallback.
fn trace_tid(task_id: TaskId) -> u64 {
    task_by_id(task_id)
        .and_then(|task| task.try_as_thread().map(|thread| thread.tid().get() as u64))
        .unwrap_or_else(|| task_id.as_u64())
}
