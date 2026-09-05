//! Pure Linux credential policy for task-targeted perf events.

/// Credential fields consumed by `PTRACE_MODE_READ_REALCREDS`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfCredentialIds {
    uid: u32,
    gid: u32,
    euid: u32,
    egid: u32,
    suid: u32,
    sgid: u32,
}

impl PerfCredentialIds {
    /// Creates one immutable real/effective/saved id snapshot.
    pub(crate) const fn new(
        uid: u32,
        gid: u32,
        euid: u32,
        egid: u32,
        suid: u32,
        sgid: u32,
    ) -> Self {
        Self {
            uid,
            gid,
            euid,
            egid,
            suid,
            sgid,
        }
    }
}

/// Capabilities that bypass the ptrace-style id checks.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfAccessCapabilities {
    perfmon_capable: bool,
    ptrace_capable: bool,
    kill_capable: bool,
}

impl PerfAccessCapabilities {
    /// Creates one capability snapshot.
    pub(crate) const fn new(
        perfmon_capable: bool,
        ptrace_capable: bool,
        kill_capable: bool,
    ) -> Self {
        Self {
            perfmon_capable,
            ptrace_capable,
            kill_capable,
        }
    }
}

/// Complete credential snapshot consumed by one perf access check.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PerfCredentialSnapshot {
    ids: PerfCredentialIds,
    capabilities: PerfAccessCapabilities,
}

impl PerfCredentialSnapshot {
    /// Creates one immutable permission-check snapshot.
    pub(crate) const fn new(ids: PerfCredentialIds, capabilities: PerfAccessCapabilities) -> Self {
        Self { ids, capabilities }
    }
}

/// Applies Linux v7.1 `perf_check_permission()` task-access semantics.
///
/// Same-thread-group targets and callers with `CAP_PERFMON` (or its
/// `CAP_SYS_ADMIN` compatibility fallback) bypass the ptrace-style check.
/// A `sigtrap` event also requires `CAP_KILL` for that capability bypass,
/// because the event can inject a signal into the target. Otherwise
/// `PTRACE_MODE_READ_REALCREDS` (or its attach variant for `sigtrap`) requires
/// the caller's real uid/gid to match every target uid/gid slot, and the
/// target must remain dumpable. `CAP_SYS_PTRACE` bypasses both the id and
/// dumpability restrictions.
pub(crate) const fn perf_task_access_allowed(
    caller: PerfCredentialSnapshot,
    target: PerfCredentialSnapshot,
    same_thread_group: bool,
    target_dumpable: bool,
    signal_delivery: bool,
) -> bool {
    let perfmon_bypass = caller.capabilities.perfmon_capable
        && (!signal_delivery || caller.capabilities.kill_capable);
    if same_thread_group || perfmon_bypass || caller.capabilities.ptrace_capable {
        return true;
    }

    target_dumpable
        && caller.ids.uid == target.ids.uid
        && caller.ids.uid == target.ids.euid
        && caller.ids.uid == target.ids.suid
        && caller.ids.gid == target.ids.gid
        && caller.ids.gid == target.ids.egid
        && caller.ids.gid == target.ids.sgid
}
