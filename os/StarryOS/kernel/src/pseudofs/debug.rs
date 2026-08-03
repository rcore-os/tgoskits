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
        "remote_wake_publications {}",
        task.remote_wake_publications
    )
    .unwrap();
    writeln!(
        output,
        "remote_wake_head_transitions {}",
        task.remote_wake_head_transitions
    )
    .unwrap();
    writeln!(
        output,
        "remote_wake_messages_drained {}",
        task.remote_wake_messages_drained
    )
    .unwrap();
    writeln!(
        output,
        "remote_wake_activations {}",
        task.remote_wake_activations
    )
    .unwrap();
    writeln!(
        output,
        "remote_wake_owner_enqueues {}",
        task.remote_wake_owner_enqueues
    )
    .unwrap();
    writeln!(
        output,
        "remote_wake_migration_handoffs {}",
        task.remote_wake_migration_handoffs
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
                "remote_wake_publications",
                "remote_wake_head_transitions",
                "remote_wake_messages_drained",
                "remote_wake_activations",
                "remote_wake_owner_enqueues",
                "remote_wake_migration_handoffs",
                "scheduler_ipi_sends",
                "scheduler_ipi_consumes",
                "clockevent_irqs",
                "context_switches",
            ]
        );
    }
}
