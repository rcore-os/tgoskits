use alloc::sync::Arc;

#[cfg(feature = "qperf-metrics")]
use super::SimpleFile;
use super::{DirMaker, DirMapping, SimpleDir, SimpleFs};

const DEBUGFS_MAGIC: u32 = 0x64626720;

/// Create a new debugfs filesystem.
pub fn new_debugfs() -> axfs_ng_vfs::Filesystem {
    // TODO: update fs_type
    SimpleFs::new_with("debug".into(), DEBUGFS_MAGIC, debugfs_builder)
}

fn debugfs_builder(fs: Arc<SimpleFs>) -> DirMaker {
    let mut root = DirMapping::new();
    let tracing = crate::tracepoint::init_tracing_dir(fs.clone());
    root.add("tracing", tracing);
    #[cfg(feature = "qperf-metrics")]
    root.add(
        "scheduler_metrics",
        SimpleFile::new_regular(fs.clone(), || Ok(render_scheduler_metrics())),
    );
    SimpleDir::new_maker(fs, Arc::new(root))
}

#[cfg(feature = "qperf-metrics")]
fn render_scheduler_metrics() -> alloc::string::String {
    use core::fmt::Write;

    let metrics = ax_runtime::task::qperf_runtime_scheduler_metrics_snapshot();
    let task = metrics.task;
    let mut output = alloc::string::String::new();
    writeln!(
        output,
        "current_thread_handle_queries {}",
        task.current_thread_handle_queries
    )
    .unwrap();
    writeln!(
        output,
        "runtime_preempt_guard_entries {}",
        task.runtime_preempt_guard_entries
    )
    .unwrap();
    writeln!(
        output,
        "runtime_preempt_guard_none {}",
        task.runtime_preempt_guard_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_ticket_entries {}",
        task.preempt_guard_ticket_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_ticket_none {}",
        task.preempt_guard_ticket_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_explicit_entries {}",
        task.preempt_guard_explicit_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_explicit_none {}",
        task.preempt_guard_explicit_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_sync_entries {}",
        task.preempt_guard_sync_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_sync_none {}",
        task.preempt_guard_sync_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_activity_entries {}",
        task.preempt_guard_activity_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_activity_none {}",
        task.preempt_guard_activity_none
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_irq_return_entries {}",
        task.preempt_guard_irq_return_entries
    )
    .unwrap();
    writeln!(
        output,
        "preempt_guard_irq_return_none {}",
        task.preempt_guard_irq_return_none
    )
    .unwrap();
    writeln!(
        output,
        "runtime_irq_guard_entries {}",
        task.runtime_irq_guard_entries
    )
    .unwrap();
    writeln!(
        output,
        "runtime_irq_guard_none {}",
        task.runtime_irq_guard_none
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_ticket_entries {}",
        task.irq_guard_ticket_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_ticket_none {}",
        task.irq_guard_ticket_none
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_thread_sched_entries {}",
        task.irq_ticket_thread_sched_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_deadline_server_entries {}",
        task.irq_ticket_deadline_server_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_run_queue_entries {}",
        task.irq_ticket_cpu_run_queue_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_rt_bandwidth_entries {}",
        task.irq_ticket_cpu_rt_bandwidth_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_cpu_deadline_entries {}",
        task.irq_ticket_cpu_deadline_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_root_rt_runtime_entries {}",
        task.irq_ticket_root_rt_runtime_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_root_rt_period_entries {}",
        task.irq_ticket_root_rt_period_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_ticket_root_deadline_index_entries {}",
        task.irq_ticket_root_deadline_index_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_explicit_entries {}",
        task.irq_guard_explicit_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_explicit_none {}",
        task.irq_guard_explicit_none
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_runtime_cpu_entries {}",
        task.irq_guard_runtime_cpu_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_runtime_cpu_none {}",
        task.irq_guard_runtime_cpu_none
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_executor_entries {}",
        task.irq_guard_executor_entries
    )
    .unwrap();
    writeln!(
        output,
        "irq_guard_executor_none {}",
        task.irq_guard_executor_none
    )
    .unwrap();
    writeln!(
        output,
        "owner_rq_irqsave_transactions {}",
        task.owner_rq_irqsave_transactions
    )
    .unwrap();
    writeln!(
        output,
        "owner_rq_scheduler_transactions {}",
        task.owner_rq_scheduler_transactions
    )
    .unwrap();
    writeln!(
        output,
        "owner_rq_bootstrap_transactions {}",
        task.owner_rq_bootstrap_transactions
    )
    .unwrap();
    writeln!(output, "direct_wake_attempts {}", task.direct_wake_attempts).unwrap();
    writeln!(
        output,
        "direct_wake_activations {}",
        task.direct_wake_activations
    )
    .unwrap();
    writeln!(output, "direct_wake_enqueues {}", task.direct_wake_enqueues).unwrap();
    writeln!(
        output,
        "direct_wake_preemptions {}",
        task.direct_wake_preemptions
    )
    .unwrap();
    writeln!(
        output,
        "direct_wake_current_kept {}",
        task.direct_wake_current_kept
    )
    .unwrap();
    writeln!(
        output,
        "direct_wake_queued_candidate_selected {}",
        task.direct_wake_queued_candidate_selected
    )
    .unwrap();
    writeln!(
        output,
        "task_work_publish_calls {}",
        task.task_work_publish_calls
    )
    .unwrap();
    writeln!(
        output,
        "task_work_publish_edges {}",
        task.task_work_publish_edges
    )
    .unwrap();
    writeln!(
        output,
        "task_work_pending_consumed {}",
        task.task_work_pending_consumed
    )
    .unwrap();
    writeln!(
        output,
        "task_work_reassertions {}",
        task.task_work_reassertions
    )
    .unwrap();
    writeln!(
        output,
        "task_work_worker_passes {}",
        task.task_work_worker_passes
    )
    .unwrap();
    writeln!(
        output,
        "task_work_worker_processed {}",
        task.task_work_worker_processed
    )
    .unwrap();
    writeln!(
        output,
        "task_work_worker_yields {}",
        task.task_work_worker_yields
    )
    .unwrap();
    writeln!(
        output,
        "task_work_worker_waits {}",
        task.task_work_worker_waits
    )
    .unwrap();
    writeln!(
        output,
        "task_work_deadline_events {}",
        task.task_work_deadline_events
    )
    .unwrap();
    writeln!(
        output,
        "task_work_scheduler_tick_events {}",
        task.task_work_scheduler_tick_events
    )
    .unwrap();
    writeln!(
        output,
        "task_work_exit_callbacks {}",
        task.task_work_exit_callbacks
    )
    .unwrap();
    writeln!(
        output,
        "task_work_reaped_threads {}",
        task.task_work_reaped_threads
    )
    .unwrap();
    writeln!(
        output,
        "task_work_coroutine_reclaims {}",
        task.task_work_coroutine_reclaims
    )
    .unwrap();
    writeln!(
        output,
        "task_work_address_space_reclaims {}",
        task.task_work_address_space_reclaims
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_ipi_sends {}",
        metrics.scheduler_ipi_sends
    )
    .unwrap();
    writeln!(
        output,
        "scheduler_ipi_consumes {}",
        metrics.scheduler_ipi_consumes
    )
    .unwrap();
    writeln!(output, "clockevent_irqs {}", metrics.clockevent_irqs).unwrap();
    writeln!(output, "context_switches {}", task.context_switches).unwrap();
    writeln!(
        output,
        "context_switches_preempted {}",
        task.context_switches_preempted
    )
    .unwrap();
    writeln!(
        output,
        "context_switches_yield {}",
        task.context_switches_yield
    )
    .unwrap();
    writeln!(
        output,
        "context_switches_blocked {}",
        task.context_switches_blocked
    )
    .unwrap();
    writeln!(
        output,
        "context_switches_exited {}",
        task.context_switches_exited
    )
    .unwrap();
    writeln!(
        output,
        "context_switches_migrated {}",
        task.context_switches_migrated
    )
    .unwrap();
    output
}

#[cfg(all(test, feature = "qperf-metrics"))]
mod tests {
    #[test]
    fn scheduler_metrics_are_machine_readable() {
        let output = super::render_scheduler_metrics();
        let keys = output
            .lines()
            .map(|line| line.split_once(' ').unwrap().0)
            .collect::<alloc::vec::Vec<_>>();

        assert_eq!(
            keys,
            [
                "current_thread_handle_queries",
                "runtime_preempt_guard_entries",
                "runtime_preempt_guard_none",
                "preempt_guard_ticket_entries",
                "preempt_guard_ticket_none",
                "preempt_guard_explicit_entries",
                "preempt_guard_explicit_none",
                "preempt_guard_sync_entries",
                "preempt_guard_sync_none",
                "preempt_guard_activity_entries",
                "preempt_guard_activity_none",
                "preempt_guard_irq_return_entries",
                "preempt_guard_irq_return_none",
                "runtime_irq_guard_entries",
                "runtime_irq_guard_none",
                "irq_guard_ticket_entries",
                "irq_guard_ticket_none",
                "irq_ticket_thread_sched_entries",
                "irq_ticket_deadline_server_entries",
                "irq_ticket_cpu_run_queue_entries",
                "irq_ticket_cpu_rt_bandwidth_entries",
                "irq_ticket_cpu_deadline_entries",
                "irq_ticket_root_rt_runtime_entries",
                "irq_ticket_root_rt_period_entries",
                "irq_ticket_root_deadline_index_entries",
                "irq_guard_explicit_entries",
                "irq_guard_explicit_none",
                "irq_guard_runtime_cpu_entries",
                "irq_guard_runtime_cpu_none",
                "irq_guard_executor_entries",
                "irq_guard_executor_none",
                "owner_rq_irqsave_transactions",
                "owner_rq_scheduler_transactions",
                "owner_rq_bootstrap_transactions",
                "direct_wake_attempts",
                "direct_wake_activations",
                "direct_wake_enqueues",
                "direct_wake_preemptions",
                "direct_wake_current_kept",
                "direct_wake_queued_candidate_selected",
                "task_work_publish_calls",
                "task_work_publish_edges",
                "task_work_pending_consumed",
                "task_work_reassertions",
                "task_work_worker_passes",
                "task_work_worker_processed",
                "task_work_worker_yields",
                "task_work_worker_waits",
                "task_work_deadline_events",
                "task_work_scheduler_tick_events",
                "task_work_exit_callbacks",
                "task_work_reaped_threads",
                "task_work_coroutine_reclaims",
                "task_work_address_space_reclaims",
                "scheduler_ipi_sends",
                "scheduler_ipi_consumes",
                "clockevent_irqs",
                "context_switches",
                "context_switches_preempted",
                "context_switches_yield",
                "context_switches_blocked",
                "context_switches_exited",
                "context_switches_migrated",
            ]
        );
    }
}
