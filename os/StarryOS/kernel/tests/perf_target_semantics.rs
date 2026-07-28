//! Behavioral target-selection contract for `perf_event_open(2)`.

#[path = "../src/perf/target.rs"]
mod target;

use target::{PerfCpuId, PerfOpenFlags, PerfTarget, PerfTaskTarget};

#[test]
fn linux_perf_target_matrix_distinguishes_task_and_cpu_contexts() {
    assert_eq!(
        PerfTarget::parse(0, -1, 4).unwrap(),
        PerfTarget::Task {
            task: PerfTaskTarget::Current,
            cpu: None,
        }
    );
    assert_eq!(
        PerfTarget::parse(42, 2, 4).unwrap(),
        PerfTarget::Task {
            task: PerfTaskTarget::Tid(42),
            cpu: Some(PerfCpuId::new(2)),
        }
    );
    assert_eq!(
        PerfTarget::parse(-1, 3, 4).unwrap(),
        PerfTarget::Cpu(PerfCpuId::new(3))
    );
}

#[test]
fn linux_perf_target_matrix_rejects_invalid_tuples() {
    for (pid, cpu) in [(-1, -1), (-2, 0), (0, -2), (1, 4)] {
        assert!(
            PerfTarget::parse(pid, cpu, 4).is_err(),
            "pid={pid}, cpu={cpu} must be rejected"
        );
    }
}

#[test]
fn linux_perf_open_flags_reject_unknown_bits_without_truncation() {
    let supported = (PerfOpenFlags::FD_NO_GROUP
        | PerfOpenFlags::FD_OUTPUT
        | PerfOpenFlags::PID_CGROUP
        | PerfOpenFlags::FD_CLOEXEC) as u64;
    assert_eq!(PerfOpenFlags::parse(supported).unwrap().bits(), 0xf);
    assert!(PerfOpenFlags::parse(1 << 4).is_err());
    assert!(PerfOpenFlags::parse(1 << 32).is_err());
}
