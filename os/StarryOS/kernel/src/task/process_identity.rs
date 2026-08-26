//! Process lifecycle operations backed by the unified PID identity index.

use alloc::{sync::Arc, vec::Vec};

use super::{
    Cred, PidIdentity, PidIdentityId, PidView, Process, ProcessCpuTime, ProcessData, Tgid,
    TgidNumber, TidNumber, ZombieSnapshot, current_user_task,
};
use crate::{StarryError, StarryResult, task::ROOT_PID_NS};

fn root_identity(tgid: TgidNumber) -> StarryResult<Arc<PidIdentity>> {
    ROOT_PID_NS
        .lookup(tgid.pid_number())
        .filter(|identity| identity.has_role::<Tgid>())
        .ok_or(StarryError::NoSuchProcess)
}

/// Returns the PID view fixed to the calling thread's active namespace.
pub(crate) fn current_pid_view() -> PidView {
    PidView::new(current_user_task().as_thread().active_pid_namespace())
}

/// Resolves one typed process number in the calling thread's active PID view.
pub(crate) fn resolve_user_process_identity_by_number(
    tgid: TgidNumber,
) -> StarryResult<Arc<PidIdentity>> {
    current_pid_view().resolve_process(tgid)
}

/// Finds live process resources by typed TGID in the calling thread's PID view.
pub(crate) fn get_user_process_data_by_number(tgid: TgidNumber) -> StarryResult<Arc<ProcessData>> {
    resolve_user_process_identity_by_number(tgid)?
        .live_data()
        .ok_or(StarryError::NoSuchProcess)
}

/// Lists live process runtime resources from the root PID index.
pub fn processes() -> Vec<Arc<ProcessData>> {
    ROOT_PID_NS
        .published_members()
        .into_iter()
        .filter(|identity| identity.has_role::<Tgid>())
        .filter_map(|identity| identity.live_data())
        .collect()
}

/// Finds live process resources by typed TGID in the root PID view.
pub(crate) fn get_process_data_by_number(tgid: TgidNumber) -> StarryResult<Arc<ProcessData>> {
    root_identity(tgid)?
        .live_data()
        .ok_or(StarryError::NoSuchProcess)
}

pub(crate) fn publish_zombie(
    proc_data: &Arc<ProcessData>,
    zombie: ZombieSnapshot,
) -> StarryResult<()> {
    proc_data
        .identity()
        .publish_zombie(proc_data, zombie)
        .map_err(|_| StarryError::BadState)
}

/// Reaps one exact generation. The leader TID and TGID leases are released
/// only after topology retirement, outside its locks, so the number cannot be
/// reused mid-retire.
pub(crate) fn reap_process(process: &Arc<Process>) -> Option<ProcessCpuTime> {
    let identity = process.identity();
    let zombie = identity.claim_reap(process)?;

    #[cfg(all(test, axtest))]
    reap_test_support::reap_claim_barrier(process.pid());

    process.retire();
    identity.finish_reap();
    let cpu_time = zombie.cpu_time;
    let tid_lease = zombie.tid_lease;
    let tgid_lease = zombie.tgid_lease;
    unsafe {
        identity
            .process_exit_event()
            .wake(axpoll::IoEvents::IN | axpoll::IoEvents::RDNORM | axpoll::IoEvents::HUP);
    }
    tid_lease.release();
    tgid_lease.release();
    Some(cpu_time)
}

pub(crate) fn is_zombie_process(process: &Arc<Process>) -> bool {
    process.identity().is_zombie()
}

pub(crate) fn is_reaped_process(process: &Arc<Process>) -> bool {
    process.identity().is_reaped()
}

fn is_live_process(process: &Arc<Process>) -> bool {
    process.identity().live_data().is_some()
}

pub(crate) fn orphan_reaper_for(process: &Arc<Process>) -> Arc<Process> {
    let namespace = process.identity().active_namespace();
    let namespace_init_identity = namespace
        .init_identity()
        .expect("active PID namespace must retain its init identity");
    let namespace_init = namespace
        .lookup_identity(namespace_init_identity)
        .expect("active PID namespace must retain its published init identity")
        .process();
    let mut cursor = process.parent();
    while let Some(candidate) = cursor {
        if Arc::ptr_eq(&candidate, &namespace_init) {
            break;
        }
        let candidate_identity = candidate.identity();
        if !Arc::ptr_eq(&candidate_identity.active_namespace(), &namespace) {
            break;
        }
        if candidate.is_child_subreaper()
            && candidate.accepts_child_publication()
            && is_live_process(&candidate)
        {
            return candidate;
        }
        cursor = candidate.parent();
    }
    namespace_init
}

pub(crate) fn get_zombie_cred(process: &Process) -> Option<Arc<Cred>> {
    process
        .identity()
        .zombie_snapshot(|zombie| zombie.cred.clone())
}

pub(crate) fn is_zombie_clone_child(process: &Process) -> Option<bool> {
    process
        .identity()
        .zombie_snapshot(|zombie| zombie.is_clone_child)
}

pub(crate) fn zombie_wait_parent_tid(process: &Process) -> Option<TidNumber> {
    process
        .identity()
        .zombie_snapshot(|zombie| zombie.wait_parent_tid)
}

pub(crate) fn traced_zombies_for(tracer: PidIdentityId) -> Vec<Arc<Process>> {
    ROOT_PID_NS
        .published_members()
        .into_iter()
        .filter(|identity| {
            identity
                .zombie_snapshot(|zombie| {
                    zombie
                        .ptrace_tracer
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.identity_id() == tracer)
                })
                .is_some_and(|matches| matches)
        })
        .map(|identity| identity.process())
        .collect()
}

#[cfg(all(test, axtest))]
mod reap_test_support {
    use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    use axpoll_set::PollSet;

    use super::*;
    #[cfg(axtest)]
    use crate::sync::IrqMutex;
    use crate::task::{PidReservation, PidReservationKind, Tid};

    static REAP_CLAIM_BARRIER_PID: AtomicU32 = AtomicU32::new(0);
    static REAP_CLAIM_REACHED: AtomicBool = AtomicBool::new(false);
    static REAP_CLAIM_RELEASED: AtomicBool = AtomicBool::new(false);

    pub(super) fn reap_claim_barrier(tgid: TgidNumber) {
        if REAP_CLAIM_BARRIER_PID.load(Ordering::Acquire) != tgid.get() {
            return;
        }
        REAP_CLAIM_REACHED.store(true, Ordering::Release);
        while !REAP_CLAIM_RELEASED.load(Ordering::Acquire) {
            ax_std::thread::yield_now();
        }
    }

    #[cfg(axtest)]
    pub(super) fn reaping_identity_is_not_publicly_resolvable_for_test() -> bool {
        let identity = PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let tid_lease = identity.acquire_role::<Tid>().unwrap();
        let tgid_lease = identity.acquire_role::<Tgid>().unwrap();
        let process = Process::new_for_axtest(identity.clone());
        let test_tgid = process.pid();
        let exit_path = identity.mark_task_exited();
        identity.bind_zombie_for_axtest(
            process.clone(),
            Arc::new(PollSet::new()),
            ZombieSnapshot {
                cred: Arc::new(Cred::default()),
                nice: 0,
                ptrace_tracer: None,
                is_clone_child: false,
                wait_parent_tid: TidNumber::from(test_tgid.pid_number()),
                cpu_time: ProcessCpuTime::default(),
                tid_lease,
                tgid_lease,
            },
        );
        exit_path.complete();

        REAP_CLAIM_REACHED.store(false, Ordering::Release);
        REAP_CLAIM_RELEASED.store(false, Ordering::Release);
        REAP_CLAIM_BARRIER_PID.store(test_tgid.get(), Ordering::Release);

        let reaped_cpu_time = Arc::new(IrqMutex::new(None));
        let reap_task = {
            let process = process.clone();
            let reaped_cpu_time = reaped_cpu_time.clone();
            ax_std::thread::spawn(move || {
                *reaped_cpu_time.lock() = reap_process(&process);
            })
        };

        while !REAP_CLAIM_REACHED.load(Ordering::Acquire) {
            ax_std::thread::yield_now();
        }
        let number = test_tgid.pid_number();
        let namespace_lookup = ROOT_PID_NS.lookup(number);
        let process_lookup =
            PidView::new(ROOT_PID_NS.clone()).resolve_process(TgidNumber::from(number));
        let identity_process_lookup = identity.public_process();

        REAP_CLAIM_RELEASED.store(true, Ordering::Release);
        reap_task.join().unwrap();
        REAP_CLAIM_BARRIER_PID.store(0, Ordering::Release);

        let group_and_session_number_retained = namespace_lookup
            .as_ref()
            .is_some_and(|registered| registered.id() == identity.id());
        let view_hidden = matches!(process_lookup, Err(StarryError::NoSuchProcess));
        let identity_hidden = matches!(identity_process_lookup, Err(StarryError::NoSuchProcess));
        let reaped_once = *reaped_cpu_time.lock() == Some(ProcessCpuTime::default());
        group_and_session_number_retained && view_hidden && identity_hidden && reaped_once
    }

    #[cfg(axtest)]
    pub(super) fn shutdown_wait_covers_the_exit_path_after_runtime_detach_for_test() -> bool {
        let identity = PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let tid_lease = identity.acquire_role::<Tid>().unwrap();

        // The runtime link detaches early in `do_exit`; zombie publication,
        // parent notification, and relation close follow. A PID namespace
        // shutdown wait must still count the member as unexited in that
        // window, mirroring Linux's `pid_allocated` dropping only in
        // `free_pid()` after `do_notify_parent()`.
        let exit_path = identity.mark_task_exited();
        let pending_after_detach = identity.has_unexited_task();

        // The PID slot must also stay published while the exit path is
        // pending, even after the thread role lease is released mid-exit (a
        // non-leader thread executing the last process exit): `begin_shutdown`
        // proves the executor is a namespace member through this slot, and
        // Linux frees the PID only in `free_pid()`.
        tid_lease.release();
        let slot_retained_while_pending = ROOT_PID_NS.retains_identity_slot_for_test(identity.id());
        let roles_released_keeps_member = identity.has_unexited_task();

        exit_path.complete();
        let complete_after_finish = !identity.has_unexited_task();
        let slot_released_after_finish = !ROOT_PID_NS.retains_identity_slot_for_test(identity.id());

        pending_after_detach
            && slot_retained_while_pending
            && roles_released_keeps_member
            && complete_after_finish
            && slot_released_after_finish
    }

    pub(super) fn reaped_process_handle_retains_exact_identity_for_test() -> bool {
        let namespace = crate::task::new_test_pid_namespace();
        let parent_identity =
            PidReservation::reserve(&namespace, PidReservationKind::ProcessLeader)
                .unwrap()
                .publish()
                .unwrap();
        let _parent_tid = parent_identity.acquire_role::<Tid>().unwrap();
        let _parent_tgid = parent_identity.acquire_role::<Tgid>().unwrap();
        let parent = Process::new_for_axtest(parent_identity);

        let child_identity = PidReservation::reserve(&namespace, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let child_id = child_identity.id();
        let child_tid = child_identity.acquire_role::<Tid>().unwrap();
        let child_tgid = child_identity.acquire_role::<Tgid>().unwrap();
        let child = parent
            .prepare_fork(child_identity.clone())
            .publish()
            .unwrap()
            .commit();
        let exit_path = child_identity.mark_task_exited();
        child_identity.bind_zombie_for_axtest(
            child.clone(),
            Arc::new(PollSet::new()),
            ZombieSnapshot {
                cred: Arc::new(Cred::default()),
                nice: 0,
                ptrace_tracer: None,
                is_clone_child: false,
                wait_parent_tid: TidNumber::from(parent.pid().pid_number()),
                cpu_time: ProcessCpuTime::default(),
                tid_lease: child_tid,
                tgid_lease: child_tgid,
            },
        );
        exit_path.complete();

        let reaped = reap_process(&child) == Some(ProcessCpuTime::default());
        drop(child_identity);
        reaped && child.identity().id() == child_id
    }
}

#[cfg(all(test, axtest))]
mod tests {
    use axpoll_set::PollSet;

    use super::*;
    use crate::task::{PidReservation, PidReservationKind, Tid};

    #[axtest::axtest]
    fn reaped_child_process_handle_retains_its_exact_identity() {
        assert!(reap_test_support::reaped_process_handle_retains_exact_identity_for_test());
    }

    #[axtest::axtest]
    fn reaping_identity_is_not_publicly_resolvable() {
        assert!(reap_test_support::reaping_identity_is_not_publicly_resolvable_for_test());
    }

    #[axtest::axtest]
    fn shutdown_wait_covers_exit_after_runtime_detach() {
        assert!(
            reap_test_support::shutdown_wait_covers_the_exit_path_after_runtime_detach_for_test()
        );
    }

    #[axtest::axtest]
    fn reaping_releases_process_owned_group_and_session_roles() {
        let namespace = crate::task::new_test_pid_namespace();
        let identity = PidReservation::reserve(&namespace, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let number = identity.root_number();
        let tid = identity.acquire_role::<Tid>().unwrap();
        let tgid = identity.acquire_role::<Tgid>().unwrap();
        let process = Process::new_for_axtest(identity.clone());

        let exit_path = identity.mark_task_exited();
        identity.bind_zombie_for_axtest(
            process.clone(),
            Arc::new(PollSet::new()),
            ZombieSnapshot {
                cred: Arc::new(Cred::default()),
                nice: 0,
                ptrace_tracer: None,
                is_clone_child: false,
                wait_parent_tid: TidNumber::from(number),
                cpu_time: ProcessCpuTime::default(),
                tid_lease: tid,
                tgid_lease: tgid,
            },
        );
        exit_path.complete();

        assert_eq!(reap_process(&process), Some(ProcessCpuTime::default()));
        assert!(namespace.lookup(number).is_some());

        drop(process);
        assert!(namespace.lookup(number).is_none());
    }

    #[axtest::axtest]
    fn shutdown_wait_covers_the_exit_path_after_runtime_detach() {
        let namespace = crate::task::new_test_pid_namespace();
        let identity = PidReservation::reserve(&namespace, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let _tid_lease = identity.acquire_role::<Tid>().unwrap();

        // Before the exit path starts, the member counts as unexited.
        assert!(identity.has_unexited_task());

        // The runtime link detaches early in `do_exit`, but zombie
        // publication, parent notification, and relation close are still
        // ahead: the member must keep counting as unexited, or a PID
        // namespace shutdown wait races those phases, finishes the
        // namespace, and the dying member panics dereferencing it. Linux
        // drops `pid_allocated` only in `free_pid()`, after
        // `do_notify_parent()`.
        let exit_path = identity.mark_task_exited();
        assert!(identity.has_unexited_task());

        exit_path.complete();
        assert!(!identity.has_unexited_task());
    }

    #[axtest::axtest]
    fn zombie_retains_the_leader_tid_role_until_reap() {
        let namespace = crate::task::new_test_pid_namespace();
        let identity = PidReservation::reserve(&namespace, PidReservationKind::ProcessLeader)
            .unwrap()
            .publish()
            .unwrap();
        let number = identity.root_number();
        let tid = identity.acquire_role::<Tid>().unwrap();
        let tgid = identity.acquire_role::<Tgid>().unwrap();
        let process = Process::new_for_axtest(identity.clone());

        let exit_path = identity.mark_task_exited();
        identity.bind_zombie_for_axtest(
            process.clone(),
            Arc::new(PollSet::new()),
            ZombieSnapshot {
                cred: Arc::new(Cred::default()),
                nice: 0,
                ptrace_tracer: None,
                is_clone_child: false,
                wait_parent_tid: TidNumber::from(number),
                cpu_time: ProcessCpuTime::default(),
                tid_lease: tid,
                tgid_lease: tgid,
            },
        );
        exit_path.complete();

        assert!(
            identity.has_role::<Tid>(),
            "a waitable zombie must own the leader TID role until reap"
        );
        assert_eq!(reap_process(&process), Some(ProcessCpuTime::default()));
        assert!(!identity.has_role::<Tid>());
    }
}
