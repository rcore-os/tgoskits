//! Resolution and authorization of `perf_event_open(2)` ownership targets.

use alloc::sync::Arc;

use ax_errno::{AxError, AxResult};

use super::{
    access_policy::{
        PerfAccessCapabilities, PerfCredentialIds, PerfCredentialSnapshot, perf_task_access_allowed,
    },
    target::{PerfCpuId, PerfTarget, PerfTargetError, PerfTargetKind, PerfTaskTarget},
};
use crate::task::{Cred, UserTaskRef, current_user_task, get_task};

/// Strong target identity with its CPU selector validated in Linux order.
pub(super) enum ResolvedPerfTarget {
    /// A live task plus its optional CPU filter.
    Task {
        task: UserTaskRef,
        cpu: Option<PerfCpuId>,
    },
    /// A system-wide event owned by one logical CPU.
    Cpu(PerfCpuId),
}

/// Strong, authorized ownership target retained through event construction.
pub(crate) enum AuthorizedPerfTarget {
    /// A live task plus its optional CPU filter.
    Task {
        task: UserTaskRef,
        cpu: Option<PerfCpuId>,
    },
    /// A system-wide event owned by one logical CPU.
    Cpu(PerfCpuId),
}

impl ResolvedPerfTarget {
    /// Returns the owner class without exposing the resolved task lease.
    pub(super) const fn kind(&self) -> PerfTargetKind {
        match self {
            Self::Task { .. } => PerfTargetKind::Task,
            Self::Cpu(_) => PerfTargetKind::Cpu,
        }
    }

    /// Resolves task identity before validating its CPU filter.
    pub(super) fn resolve(target: PerfTarget, cpu_count: usize) -> AxResult<Self> {
        match target {
            PerfTarget::Task { task, cpu } => {
                let task = match task {
                    PerfTaskTarget::Current => current_user_task(),
                    PerfTaskTarget::Tid(tid) => get_task(tid)?,
                };
                let cpu = cpu
                    .resolve_optional(cpu_count)
                    .map_err(target_error_to_ax)?;
                Ok(Self::Task { task, cpu })
            }
            PerfTarget::Cpu(cpu) => Ok(Self::Cpu(
                cpu.resolve_required(cpu_count)
                    .map_err(target_error_to_ax)?,
            )),
        }
    }

    /// Serializes authorization and installation against target exec.
    ///
    /// Linux retains `signal->exec_update_lock` from
    /// `perf_check_permission()` through `perf_install_in_context()`. Starry's
    /// process `exec_lock` provides the corresponding lifetime boundary.
    pub(super) fn with_authorized<R>(
        self,
        signal_delivery: bool,
        install: impl FnOnce(AuthorizedPerfTarget) -> AxResult<R>,
    ) -> AxResult<R> {
        let (task, cpu) = match self {
            Self::Task { task, cpu } => (task, cpu),
            Self::Cpu(cpu) => return install(AuthorizedPerfTarget::Cpu(cpu)),
        };

        let caller = current_user_task();
        let target_process = Arc::clone(&task.as_thread().proc_data);
        let _exec_guard = target_process.exec_lock().lock();
        let caller_thread = caller.as_thread();
        let target_thread = task.as_thread();
        let caller_cred = caller_thread.cred();
        let target_cred = target_thread.cred();
        let same_thread_group = Arc::ptr_eq(&caller_thread.proc_data, &target_thread.proc_data);
        let allowed = perf_task_access_allowed(
            credential_snapshot(&caller_cred),
            credential_snapshot(&target_cred),
            same_thread_group,
            target_thread.proc_data.dumpable() == 1,
            signal_delivery,
        );
        if !allowed {
            return Err(AxError::PermissionDenied);
        }

        install(AuthorizedPerfTarget::Task { task, cpu })
    }
}

fn target_error_to_ax(error: PerfTargetError) -> AxError {
    match error {
        PerfTargetError::InvalidTuple => AxError::InvalidInput,
        PerfTargetError::NoSuchProcess => AxError::NoSuchProcess,
    }
}

fn credential_snapshot(cred: &Cred) -> PerfCredentialSnapshot {
    PerfCredentialSnapshot::new(
        PerfCredentialIds::new(
            cred.uid, cred.gid, cred.euid, cred.egid, cred.suid, cred.sgid,
        ),
        PerfAccessCapabilities::new(
            cred.has_cap_perfmon(),
            cred.has_cap_sys_ptrace(),
            cred.has_cap_kill(),
        ),
    )
}
