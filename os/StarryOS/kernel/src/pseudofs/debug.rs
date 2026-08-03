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
                "scheduler_ipi_sends",
                "scheduler_ipi_consumes",
                "clockevent_irqs",
                "context_switches",
            ]
        );
    }
}
