//! Typed target parsing for `perf_event_open(2)`.

use crate::{
    StarryError, StarryResult,
    task::{AsThread, PidIdentityId, TidNumber, get_user_task_by_number},
};

/// Validated logical CPU id.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfCpuId(usize);

impl PerfCpuId {
    /// Returns the logical CPU as an array index.
    pub(crate) const fn as_usize(self) -> usize {
        self.0
    }
}

/// CPU selector retained until task lookup has established Linux error order.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfCpuRequest(i32);

impl PerfCpuRequest {
    /// Resolves an optional task CPU filter.
    pub(crate) fn resolve_optional(self, cpu_count: usize) -> Result<Option<PerfCpuId>, TargetError> {
        match self.0 {
            -1 => Ok(None),
            cpu if cpu >= 0 && (cpu as usize) < cpu_count => Ok(Some(PerfCpuId(cpu as usize))),
            _ => Err(TargetError::InvalidTuple),
        }
    }

    /// Resolves a required system-wide CPU owner.
    pub(crate) fn resolve_required(self, cpu_count: usize) -> Result<PerfCpuId, TargetError> {
        self.resolve_optional(cpu_count)?
            .ok_or(TargetError::InvalidTuple)
    }
}

/// Task selector carried by the Linux `pid` argument.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfTaskTarget {
    /// The calling task.
    Current,
    /// A TID visible in the caller's active PID namespace.
    Tid(u32),
}

/// Deferred task or CPU perf context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfTarget {
    /// A task, optionally constrained to one CPU.
    Task {
        task: PerfTaskTarget,
        cpu: PerfCpuRequest,
    },
    /// A system-wide event fixed to one CPU.
    Cpu(PerfCpuRequest),
}

/// Generation-stable context key used by group and output validation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PerfContextKey {
    /// One task scheduler context and its optional CPU constraint.
    Task {
        identity: PidIdentityId,
        cpu: Option<PerfCpuId>,
    },
    /// One fixed system-wide CPU context.
    Cpu(PerfCpuId),
}

/// Strong task or fixed-CPU target retained through event construction.
#[derive(Clone)]
pub(crate) enum ResolvedPerfTarget {
    /// One live task and its optional CPU filter.
    Task {
        task: ax_task::AxTaskRef,
        cpu: Option<PerfCpuId>,
        external_pid: i32,
    },
    /// One fixed system-wide CPU owner.
    Cpu(PerfCpuId),
}

/// Linux error class for target parsing.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TargetError {
    /// The tuple cannot name a perf context.
    InvalidTuple,
    /// A negative PID other than `-1` cannot name a task.
    NoSuchProcess,
}

impl PerfTarget {
    /// Parses PID identity before CPU validity is checked.
    pub(crate) fn parse(pid: i32, cpu: i32) -> Result<Self, TargetError> {
        if pid < -1 {
            return Err(TargetError::NoSuchProcess);
        }
        let cpu = PerfCpuRequest(cpu);
        match pid {
            -1 => Ok(Self::Cpu(cpu)),
            0 => Ok(Self::Task {
                task: PerfTaskTarget::Current,
                cpu,
            }),
            pid => Ok(Self::Task {
                task: PerfTaskTarget::Tid(pid as u32),
                cpu,
            }),
        }
    }

    /// Returns the deferred CPU selector.
    #[cfg(test)]
    pub(crate) const fn cpu_request(self) -> PerfCpuRequest {
        match self {
            Self::Task { cpu, .. } | Self::Cpu(cpu) => cpu,
        }
    }

    /// Resolves a live task before validating its CPU filter.
    pub(crate) fn resolve(self) -> StarryResult<ResolvedPerfTarget> {
        let cpu_count = ax_runtime::hal::cpu_num();
        match self {
            Self::Task { task, cpu } => {
                let (task, external_pid) = match task {
                    PerfTaskTarget::Current => (ax_task::current().clone(), 0),
                    PerfTaskTarget::Tid(tid) => {
                        let tid = TidNumber::try_from(tid)?;
                        (get_user_task_by_number(tid)?, tid.get() as i32)
                    }
                };
                let cpu = cpu.resolve_optional(cpu_count).map_err(StarryError::from)?;
                Ok(ResolvedPerfTarget::Task {
                    task,
                    cpu,
                    external_pid,
                })
            }
            Self::Cpu(cpu) => Ok(ResolvedPerfTarget::Cpu(
                cpu.resolve_required(cpu_count).map_err(StarryError::from)?,
            )),
        }
    }
}

impl ResolvedPerfTarget {
    /// Returns a generation-stable group/output context.
    pub(crate) fn context_key(&self) -> PerfContextKey {
        match self {
            Self::Task { task, cpu, .. } => PerfContextKey::Task {
                identity: task.as_thread().pid_identity().id(),
                cpu: *cpu,
            },
            Self::Cpu(cpu) => PerfContextKey::Cpu(*cpu),
        }
    }

    /// Returns the external PID expected by `kbpf_basic`.
    pub(crate) const fn external_pid(&self) -> i32 {
        match self {
            Self::Task { external_pid, .. } => *external_pid,
            Self::Cpu(_) => -1,
        }
    }

    /// Returns the validated CPU argument.
    pub(crate) const fn cpu(&self) -> i32 {
        match self {
            Self::Task { cpu, .. } => match cpu {
                Some(cpu) => cpu.as_usize() as i32,
                None => -1,
            },
            Self::Cpu(cpu) => cpu.as_usize() as i32,
        }
    }
}

impl From<TargetError> for StarryError {
    fn from(error: TargetError) -> Self {
        match error {
            TargetError::InvalidTuple => Self::InvalidInput,
            TargetError::NoSuchProcess => Self::NoSuchProcess,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_linux_target_forms_parse() {
        assert!(matches!(
            PerfTarget::parse(0, -1),
            Ok(PerfTarget::Task {
                task: PerfTaskTarget::Current,
                ..
            })
        ));
        assert!(matches!(PerfTarget::parse(-1, 3), Ok(PerfTarget::Cpu(_))));
        assert!(matches!(
            PerfTarget::parse(-1, -1),
            Ok(PerfTarget::Cpu(request)) if request.resolve_required(4).is_err()
        ));
        assert_eq!(PerfTarget::parse(-2, 0), Err(TargetError::NoSuchProcess));
    }

    #[test]
    fn cpu_filter_is_checked_after_pid_parsing() {
        let target = PerfTarget::parse(42, 8).unwrap();
        assert_eq!(target.cpu_request().resolve_optional(4), Err(TargetError::InvalidTuple));
    }
}
