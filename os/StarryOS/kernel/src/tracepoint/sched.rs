//! `sched:*` tracepoints.
//!
//! `sched_switch` is fired by the runtime's allocation-free scheduler trace
//! hook after `ax-task` commits a context-switch decision.
//!
//! The other two `sched:*` events are defined next to their emission sites
//! rather than here: `sched_process_fork` in `crate::syscall::task::clone`
//! and `sched_process_exit` in `crate::task::ops`. Registration is by link
//! section, so their physical location does not affect discovery.

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

pub(super) fn install() {
    ax_runtime::task::install_sched_switch_trace_hook(on_sched_switch);
}

fn on_sched_switch(record: ax_runtime::task::SchedSwitchRecord) {
    trace_sched_switch(record.previous_thread, record.next_thread, record.reason);
}
