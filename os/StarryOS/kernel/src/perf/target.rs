//! Typed ownership target for `perf_event_open(2)`.

pub(crate) use super::cpu_id::PerfCpuId;

/// Raw CPU selector retained until the target task has been resolved.
///
/// Linux resolves a positive TID before validating the optional CPU filter,
/// so this request intentionally defers range validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfCpuRequest(i32);

impl PerfCpuRequest {
    /// Creates an unresolved CPU selector from the syscall argument.
    const fn new(value: i32) -> Self {
        Self(value)
    }

    /// Resolves an optional task CPU filter.
    pub(crate) fn resolve_optional(
        self,
        cpu_count: usize,
    ) -> Result<Option<PerfCpuId>, PerfTargetError> {
        match self.0 {
            -1 => Ok(None),
            value if value >= 0 && (value as usize) < cpu_count => {
                Ok(Some(PerfCpuId::new(value as usize)))
            }
            _ => Err(PerfTargetError::InvalidTuple),
        }
    }

    /// Resolves a required system-wide CPU owner.
    pub(crate) fn resolve_required(self, cpu_count: usize) -> Result<PerfCpuId, PerfTargetError> {
        self.resolve_optional(cpu_count)?
            .ok_or(PerfTargetError::InvalidTuple)
    }
}

/// Linux error class produced while parsing a `pid`/`cpu` target tuple.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfTargetError {
    /// The tuple cannot identify a task or CPU context.
    InvalidTuple,
    /// A negative PID other than the `-1` CPU-context sentinel has no task.
    NoSuchProcess,
}

/// Task identity accepted by `perf_event_open(2)`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfTaskTarget {
    /// The calling task (`pid == 0`).
    Current,
    /// One Linux thread id (`pid > 0`).
    Tid(u32),
}

/// Runtime owner class used for target-specific event validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfTargetKind {
    /// A task scheduler context.
    Task,
    /// A fixed logical CPU context.
    Cpu,
}

/// Generation-bearing scheduler or fixed-CPU context used by event groups.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfContextKey {
    /// One scheduler thread generation and its optional CPU constraint.
    Task {
        scheduler_id: ax_runtime::task::thread::ThreadId,
        cpu: Option<PerfCpuId>,
    },
    /// One fixed system-wide CPU context.
    Cpu(PerfCpuId),
}

/// Scheduler or CPU context that owns one perf event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfTarget {
    /// A task context with a deferred optional CPU filter.
    Task {
        task: PerfTaskTarget,
        cpu: PerfCpuRequest,
    },
    /// A CPU context (`pid == -1`) with deferred CPU validation.
    Cpu(PerfCpuRequest),
}

impl PerfTarget {
    /// Parses target identity while deferring CPU validation.
    ///
    /// Deferral preserves Linux's error precedence: a missing positive TID is
    /// reported as `ESRCH` even when its CPU filter is also invalid.
    pub(crate) fn parse(pid: i32, cpu: i32) -> Result<Self, PerfTargetError> {
        if pid < -1 {
            return Err(PerfTargetError::NoSuchProcess);
        }
        let cpu = PerfCpuRequest::new(cpu);

        match pid {
            -1 => Ok(Self::Cpu(cpu)),
            0 => Ok(Self::Task {
                task: PerfTaskTarget::Current,
                cpu,
            }),
            value if value > 0 => Ok(Self::Task {
                task: PerfTaskTarget::Tid(value as u32),
                cpu,
            }),
            _ => unreachable!("negative task PIDs were rejected before CPU parsing"),
        }
    }
}

#[cfg(all(test, not(axtest)))]
mod tests {
    use super::*;

    #[test]
    fn linux_perf_target_matrix_distinguishes_task_and_cpu_contexts() {
        assert_ne!(PerfTargetKind::Task, PerfTargetKind::Cpu);
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
}
