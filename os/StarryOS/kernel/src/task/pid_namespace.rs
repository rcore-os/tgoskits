//! PID namespace publication and shutdown synchronization.

use core::sync::atomic::{AtomicU64, Ordering};

use ax_std::os::arceos::task::WaitQueue;
use ax_sync::{PiMutex, PiMutexGuard};

use super::ProcessIdentity;

pub(crate) type PidNamespaceRef = axnsproxy::PidNamespaceRef;

static MEMBERS_CHANGED: WaitQueue = WaitQueue::new();
static MEMBER_EPOCH: AtomicU64 = AtomicU64::new(0);
static PUBLICATION: PiMutex<()> = PiMutex::new(());

/// Serializes the clone no-failure publication point with namespace shutdown.
pub(crate) fn lock_publication() -> PiMutexGuard<'static, ()> {
    PUBLICATION.lock()
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
) -> alloc::vec::Vec<u64> {
    namespace.published_members_excluding(init_global_pid)
}

/// Services newly reparented zombies until every non-init identity is gone.
pub(crate) fn wait_for_victims(
    namespace: &PidNamespaceRef,
    init_global_pid: u64,
    mut service_zombies: impl FnMut(),
) {
    loop {
        // Snapshot first, then service. A terminal publication racing between
        // the two is either observed by the service pass or advances the
        // epoch checked by the predicate.
        let observed = MEMBER_EPOCH.load(Ordering::Acquire);
        service_zombies();
        if !namespace.has_members_excluding(init_global_pid) {
            return;
        }
        MEMBERS_CHANGED.wait_until(|| {
            MEMBER_EPOCH.load(Ordering::Acquire) != observed
                || !namespace.has_members_excluding(init_global_pid)
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
