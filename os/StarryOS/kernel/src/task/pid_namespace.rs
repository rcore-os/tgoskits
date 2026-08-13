//! PID namespace publication and shutdown synchronization.

use core::sync::atomic::{AtomicU64, Ordering};

use ax_errno::{AxError, AxResult};
use ax_std::os::arceos::task::WaitQueue;
use starry_process::Pid;

use super::{ProcessData, ProcessIdentity, UserTaskRef};
use crate::sync::{PiMutex, PiMutexGuard};

pub(crate) type PidNamespaceRef = axnsproxy::PidNamespaceRef;

static MEMBERS_CHANGED: WaitQueue = WaitQueue::new();
static MEMBER_EPOCH: AtomicU64 = AtomicU64::new(0);
static PUBLICATION: PiMutex<()> = PiMutex::new(());

/// Serializes the clone no-failure publication point with namespace shutdown.
pub(crate) fn lock_publication() -> PiMutexGuard<'static, ()> {
    PUBLICATION.lock()
}

/// Resolves a userspace PID in the calling task's active PID namespace.
pub(crate) fn resolve_user_pid(current: &UserTaskRef, local_pid: Pid) -> AxResult<Pid> {
    let namespace = current
        .as_thread()
        .proc_data
        .namespace_snapshot()
        .pid_ns
        .clone();
    namespace
        .global_pid(local_pid)
        .and_then(|pid| Pid::try_from(pid).ok())
        .ok_or(AxError::NoSuchProcess)
}

/// Returns a global task identity as seen from the caller's PID namespace.
pub(crate) fn visible_user_pid(current: &UserTaskRef, global_pid: u64) -> Pid {
    visible_process_pid(&current.as_thread().proc_data, global_pid)
}

/// Returns a global task identity as seen from a receiving process's PID namespace.
pub(crate) fn visible_process_pid(process: &ProcessData, global_pid: u64) -> Pid {
    process
        .namespace_snapshot()
        .pid_ns
        .local_pid(global_pid)
        .unwrap_or(0)
}

/// Starts namespace shutdown before relationship or zombie publication.
pub(crate) fn begin_shutdown(
    _publication: &PiMutexGuard<'static, ()>,
    namespace: &PidNamespaceRef,
    init_global_pid: u64,
) {
    assert!(
        namespace.begin_shutdown(init_global_pid),
        "only the active PID namespace init can start shutdown"
    );
}

/// Returns published tasks outside the namespace init thread group.
pub(crate) fn published_victim_tids(
    namespace: &PidNamespaceRef,
    init_global_pid: u64,
    reaper_global_tid: u64,
) -> alloc::vec::Vec<u64> {
    namespace.published_shutdown_victims(init_global_pid, reaper_global_tid)
}

/// Services newly reparented zombies until every non-init identity is gone.
pub(crate) fn wait_for_victims(
    namespace: &PidNamespaceRef,
    init_global_pid: u64,
    reaper_global_tid: u64,
    mut service_zombies: impl FnMut(),
) {
    loop {
        // Snapshot first, then service. A terminal publication racing between
        // the two is either observed by the service pass or advances the
        // epoch checked by the predicate.
        let observed = MEMBER_EPOCH.load(Ordering::Acquire);
        service_zombies();
        if !namespace.has_shutdown_victims(init_global_pid, reaper_global_tid) {
            return;
        }
        MEMBERS_CHANGED.wait_until(|| {
            MEMBER_EPOCH.load(Ordering::Acquire) != observed
                || !namespace.has_shutdown_victims(init_global_pid, reaper_global_tid)
        });
    }
}

/// Wakes a namespace shutdown waiter after reservation rollback.
pub(crate) fn notify_members_changed() {
    MEMBER_EPOCH.fetch_add(1, Ordering::Release);
    MEMBERS_CHANGED.notify_all();
}

/// Releases a non-leader TID after its complete thread-exit publication.
pub(crate) fn release_thread_pid(identity: &ProcessIdentity, global_tid: u64) {
    for namespace in identity.pid_namespaces() {
        let _ = namespace.release_thread_pid(global_tid);
    }
    // Even a leader owns a process PID rather than a thread PID. Its terminal
    // publication may still have reparented a zombie to a namespace init, so
    // shutdown servicing must observe every completed thread exit.
    notify_members_changed();
}

/// Releases a process PID only after its stable zombie identity is reaped.
pub(crate) fn release_process_pid(identity: &ProcessIdentity) {
    let mut released = false;
    for namespace in identity.pid_namespaces() {
        released |= namespace.release_process_pid(identity.pid() as u64);
    }
    if released {
        notify_members_changed();
    }
}
