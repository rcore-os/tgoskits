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
    fn on_sched_switch(prev_tid: u64, next_tid: u64, prev_state: u32, next_name: &str) {
        trace_sched_switch(prev_tid, next_tid, prev_state);
        // TPU 流水线可观测性：按任务累计 CPU 占用，只做原子加 / 定长拷贝，在 IRQ-off / 抢占禁的切换点安全。
        // 仅 sg2002（TPU 所在平台）编译，其它平台零开销。
        #[cfg(feature = "sg2002")]
        crate::pseudofs::dev::tpu::sched_probe::on_switch(prev_tid, next_tid, next_name);
        #[cfg(not(feature = "sg2002"))]
        let _ = next_name;
    }
}
