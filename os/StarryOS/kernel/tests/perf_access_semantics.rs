//! Linux v7.1 task-target perf permission semantics.

#[path = "../src/perf/access_policy.rs"]
mod access_policy;

use access_policy::{
    PerfAccessCapabilities, PerfCredentialIds, PerfCredentialSnapshot, perf_task_access_allowed,
};

fn credentials(uid: u32, gid: u32) -> PerfCredentialSnapshot {
    PerfCredentialSnapshot::new(
        PerfCredentialIds::new(uid, gid, uid, gid, uid, gid),
        PerfAccessCapabilities::new(false, false, false),
    )
}

#[test]
fn cross_credential_task_target_is_denied_even_when_dumpable() {
    assert!(!perf_task_access_allowed(
        credentials(1000, 1000),
        credentials(2000, 2000),
        false,
        true,
        false,
    ));
}

#[test]
fn real_credentials_must_match_every_target_id_slot() {
    let caller = credentials(1000, 1000);
    let matching = credentials(1000, 1000);
    let mixed_target = PerfCredentialSnapshot::new(
        PerfCredentialIds::new(1000, 1000, 1000, 1000, 2000, 1000),
        PerfAccessCapabilities::new(false, false, false),
    );
    assert!(perf_task_access_allowed(
        caller, matching, false, true, false
    ));
    assert!(!perf_task_access_allowed(
        caller,
        mixed_target,
        false,
        true,
        false,
    ));
    assert!(!perf_task_access_allowed(
        caller, matching, false, false, false,
    ));
}

#[test]
fn thread_group_and_capabilities_bypass_ptrace_id_matching() {
    let caller = credentials(1000, 1000);
    let target = credentials(2000, 2000);
    assert!(perf_task_access_allowed(caller, target, true, false, false));

    let perfmon = PerfCredentialSnapshot::new(
        PerfCredentialIds::new(1000, 1000, 1000, 1000, 1000, 1000),
        PerfAccessCapabilities::new(true, false, true),
    );
    assert!(perf_task_access_allowed(
        perfmon, target, false, false, false
    ));

    let ptrace = PerfCredentialSnapshot::new(
        PerfCredentialIds::new(1000, 1000, 1000, 1000, 1000, 1000),
        PerfAccessCapabilities::new(false, true, false),
    );
    assert!(perf_task_access_allowed(
        ptrace, target, false, false, false
    ));
}

#[test]
fn sigtrap_requires_kill_for_perfmon_bypass() {
    let target = credentials(2000, 2000);
    let perfmon_without_kill = PerfCredentialSnapshot::new(
        PerfCredentialIds::new(1000, 1000, 1000, 1000, 1000, 1000),
        PerfAccessCapabilities::new(true, false, false),
    );

    assert!(!perf_task_access_allowed(
        perfmon_without_kill,
        target,
        false,
        true,
        true,
    ));
}
