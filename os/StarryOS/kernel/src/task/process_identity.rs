//! Process lifecycle operations backed by the unified PID identity index.

use alloc::{sync::Arc, vec::Vec};

use ax_task::current;

use super::{
    AsThread, Cred, PidIdentity, PidIdentityId, PidView, Process, ProcessCpuTime, ProcessData,
    Tgid, TgidNumber, TidNumber, ZombieSnapshot, init_proc,
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
    PidView::new(current().as_thread().active_pid_namespace())
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

pub(crate) fn is_user_zombie_process(tgid: TgidNumber) -> bool {
    resolve_user_process_identity_by_number(tgid).is_ok_and(|identity| identity.is_zombie())
}

/// PID publication is performed by `PidReservation::publish`; this assertion
/// keeps task publication tied to the exact process identity.
pub(crate) fn register_process_identity(proc_data: &Arc<ProcessData>) {
    let identity = proc_data.identity();
    assert!(identity.has_role::<Tgid>());
    assert!(identity.matches_process(&proc_data.proc));
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

fn process_identity(process: &Arc<Process>) -> Option<Arc<PidIdentity>> {
    let identity = root_identity(process.pid_number()).ok()?;
    identity.matches_process(process).then_some(identity)
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

/// Reaps one exact generation. The TGID lease is released only after topology
/// retirement, outside its locks, so the number cannot be reused mid-retire.
pub(crate) fn reap_process(process: &Arc<Process>) -> Option<ProcessCpuTime> {
    let identity = process_identity(process)?;
    let zombie = identity.claim_reap(process)?;

    #[cfg(axtest)]
    axtest::reap_claim_barrier(process.pid());

    process.retire();
    identity.finish_reap();
    let cpu_time = zombie.cpu_time;
    let tgid_lease = zombie.tgid_lease;
    unsafe {
        identity
            .process_exit_event()
            .wake(axpoll::IoEvents::IN | axpoll::IoEvents::RDNORM | axpoll::IoEvents::HUP);
    }
    tgid_lease.release();
    Some(cpu_time)
}

pub(crate) fn is_zombie_process(process: &Arc<Process>) -> bool {
    process_identity(process).is_some_and(|identity| identity.is_zombie())
}

pub(crate) fn is_reaped_process(process: &Arc<Process>) -> bool {
    process_identity(process).is_none_or(|identity| identity.is_reaped())
}

fn is_live_process(process: &Arc<Process>) -> bool {
    process_identity(process).is_some_and(|identity| identity.live_data().is_some())
}

pub(crate) fn orphan_reaper_for(process: &Arc<Process>) -> Arc<Process> {
    let init = init_proc();
    let mut cursor = process.parent();
    while let Some(candidate) = cursor {
        if Arc::ptr_eq(&candidate, &init) {
            break;
        }
        if candidate.is_child_subreaper() && is_live_process(&candidate) {
            return candidate;
        }
        cursor = candidate.parent();
    }
    init
}

pub fn get_zombie_cred(tgid: TgidNumber) -> Option<Arc<Cred>> {
    root_identity(tgid)
        .ok()?
        .zombie_snapshot(|zombie| zombie.cred.clone())
}

pub(crate) fn is_zombie_clone_child(tgid: TgidNumber) -> Option<bool> {
    root_identity(tgid)
        .ok()?
        .zombie_snapshot(|zombie| zombie.is_clone_child)
}

pub(crate) fn zombie_wait_parent_tid(tgid: TgidNumber) -> Option<TidNumber> {
    root_identity(tgid)
        .ok()?
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

#[cfg(axtest)]
#[path = "process_identity_axtest.rs"]
mod axtest;

#[cfg(axtest)]
pub(crate) use axtest::reaping_identity_is_not_publicly_resolvable_for_test;

#[cfg(test)]
mod tests {
    use axpoll::PollSet;

    use super::*;
    use crate::task::{PidReservation, PidReservationKind, Tid};

    #[test]
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

        identity.mark_task_exited();
        tid.release();
        identity.bind_zombie_for_axtest(
            process.clone(),
            Arc::new(PollSet::new()),
            ZombieSnapshot {
                cred: Arc::new(Cred::default()),
                ptrace_tracer: None,
                is_clone_child: false,
                wait_parent_tid: TidNumber::from(number),
                cpu_time: ProcessCpuTime::default(),
                tgid_lease: tgid,
            },
        );

        assert_eq!(reap_process(&process), Some(ProcessCpuTime::default()));
        assert!(namespace.lookup(number).is_some());

        drop(process);
        assert!(namespace.lookup(number).is_none());
    }
}
