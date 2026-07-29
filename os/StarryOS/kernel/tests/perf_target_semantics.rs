//! Behavioral target-selection contract for `perf_event_open(2)`.

#[path = "../src/perf/target.rs"]
mod target;

use target::{
    PerfCpuId, PerfOpenFlags, PerfTarget, PerfTargetError, PerfTargetKind, PerfTaskTarget,
};

#[test]
fn linux_perf_target_matrix_distinguishes_task_and_cpu_contexts() {
    assert_ne!(PerfTargetKind::Task, PerfTargetKind::Cpu);
    assert_eq!(PerfCpuId::new(3).as_usize(), 3);
    let PerfTarget::Task { task, cpu } = PerfTarget::parse(0, -1).unwrap() else {
        panic!("pid 0 must select a task context");
    };
    assert_eq!(task, PerfTaskTarget::Current);
    assert_eq!(cpu.resolve_optional(4).unwrap(), None);

    let PerfTarget::Task { task, cpu } = PerfTarget::parse(42, 2).unwrap() else {
        panic!("a positive pid must select a task context");
    };
    assert_eq!(task, PerfTaskTarget::Tid(42));
    assert_eq!(cpu.resolve_optional(4).unwrap(), Some(PerfCpuId::new(2)));

    let PerfTarget::Cpu(cpu) = PerfTarget::parse(-1, 3).unwrap() else {
        panic!("pid -1 must select a CPU context");
    };
    assert_eq!(cpu.resolve_required(4).unwrap(), PerfCpuId::new(3));
}

#[test]
fn linux_perf_target_matrix_rejects_invalid_tuples() {
    assert_eq!(
        PerfTarget::parse(-2, 0),
        Err(PerfTargetError::NoSuchProcess)
    );

    let PerfTarget::Cpu(cpu) = PerfTarget::parse(-1, -1).unwrap() else {
        panic!("pid -1 must select a CPU context before CPU validation");
    };
    assert_eq!(cpu.resolve_required(4), Err(PerfTargetError::InvalidTuple));

    for (pid, cpu) in [(0, -2), (1, 4)] {
        let PerfTarget::Task { cpu, .. } = PerfTarget::parse(pid, cpu).unwrap() else {
            panic!("pid={pid} must select a task before CPU validation");
        };
        assert_eq!(cpu.resolve_optional(4), Err(PerfTargetError::InvalidTuple));
    }
}

#[test]
fn linux_perf_open_flags_reject_unknown_bits_without_truncation() {
    let supported = (PerfOpenFlags::FD_NO_GROUP
        | PerfOpenFlags::FD_OUTPUT
        | PerfOpenFlags::PID_CGROUP
        | PerfOpenFlags::FD_CLOEXEC) as u64;
    let flags = PerfOpenFlags::parse(supported).unwrap();
    assert_eq!(flags.bits(), 0xf);
    assert!(flags.contains(PerfOpenFlags::PID_CGROUP));
    assert!(PerfOpenFlags::parse(1 << 4).is_err());
    assert!(PerfOpenFlags::parse(1 << 32).is_err());
}
