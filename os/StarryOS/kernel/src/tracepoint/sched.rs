//! `sched:*` tracepoints.
//!
//! `sched_switch` is fired by `ax-task` through the cross-crate
//! [`ax_task::SchedTracepoint`] interface (gated by `tracepoint-hooks`).
//!
//! The other two `sched:*` events are defined next to their emission sites
//! rather than here: `sched_process_fork` in `crate::syscall::task::clone`
//! and `sched_process_exit` in `crate::task::ops`. Registration is by link
//! section, so their physical location does not affect discovery.

use ax_task::SchedTracepoint;

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
    fn on_sched_switch(prev_tid: u64, next_tid: u64, prev_state: u32) {
        trace_sched_switch(prev_tid, next_tid, prev_state);
    }
}
