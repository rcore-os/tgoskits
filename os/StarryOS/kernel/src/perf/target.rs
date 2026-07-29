//! Typed ownership target for `perf_event_open(2)`.

/// Validated logical CPU target for one perf event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfCpuId(usize);

impl PerfCpuId {
    /// Creates a validated logical CPU id.
    pub(crate) const fn new(value: usize) -> Self {
        Self(value)
    }

    /// Returns the CPU id as an array index.
    #[cfg(any(target_arch = "aarch64", test))]
    pub(crate) const fn as_usize(self) -> usize {
        self.0
    }
}

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

/// Validated `perf_event_open(2)` flag set.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfOpenFlags(u32);

impl PerfOpenFlags {
    /// `PERF_FLAG_FD_NO_GROUP`.
    pub(crate) const FD_NO_GROUP: u32 = 1 << 0;
    /// `PERF_FLAG_FD_OUTPUT`.
    pub(crate) const FD_OUTPUT: u32 = 1 << 1;
    /// `PERF_FLAG_PID_CGROUP`.
    pub(crate) const PID_CGROUP: u32 = 1 << 2;
    /// `PERF_FLAG_FD_CLOEXEC`.
    pub(crate) const FD_CLOEXEC: u32 = 1 << 3;
    const ALL: u64 =
        (Self::FD_NO_GROUP | Self::FD_OUTPUT | Self::PID_CGROUP | Self::FD_CLOEXEC) as u64;

    /// Parses the complete syscall-width flag word.
    pub(crate) const fn parse(flags: u64) -> Result<Self, PerfTargetError> {
        if flags & !Self::ALL != 0 {
            return Err(PerfTargetError::InvalidTuple);
        }
        Ok(Self(flags as u32))
    }

    /// Returns the validated Linux flag bits.
    pub(crate) const fn bits(self) -> u32 {
        self.0
    }

    /// Reports whether one validated flag is set.
    pub(crate) const fn contains(self, flag: u32) -> bool {
        self.0 & flag != 0
    }
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
