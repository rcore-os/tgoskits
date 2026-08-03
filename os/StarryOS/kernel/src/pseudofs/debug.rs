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
            ]
        );
    }
}
