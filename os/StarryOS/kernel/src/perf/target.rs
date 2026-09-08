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
