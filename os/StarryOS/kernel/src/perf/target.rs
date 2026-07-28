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

/// Invalid Linux `pid`/`cpu` target tuple.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct InvalidPerfTarget;

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
    pub(crate) const fn parse(flags: u64) -> Result<Self, InvalidPerfTarget> {
        if flags & !Self::ALL != 0 {
            return Err(InvalidPerfTarget);
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

/// Scheduler or CPU context that owns one perf event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfTarget {
    /// A task context, optionally constrained to one CPU.
    Task {
        task: PerfTaskTarget,
        cpu: Option<PerfCpuId>,
    },
    /// A CPU context (`pid == -1`, `cpu >= 0`).
    Cpu(PerfCpuId),
}

impl PerfTarget {
    /// Parses the Linux `pid`/`cpu` target tuple.
    pub(crate) fn parse(pid: i32, cpu: i32, cpu_count: usize) -> Result<Self, InvalidPerfTarget> {
        let cpu = match cpu {
            -1 => None,
            value if value >= 0 && (value as usize) < cpu_count => {
                Some(PerfCpuId::new(value as usize))
            }
            _ => return Err(InvalidPerfTarget),
        };

        match pid {
            -1 => cpu.map(Self::Cpu).ok_or(InvalidPerfTarget),
            0 => Ok(Self::Task {
                task: PerfTaskTarget::Current,
                cpu,
            }),
            value if value > 0 => Ok(Self::Task {
                task: PerfTaskTarget::Tid(value as u32),
                cpu,
            }),
            _ => Err(InvalidPerfTarget),
        }
    }
}
