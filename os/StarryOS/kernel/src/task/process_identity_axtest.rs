//! Deterministic concurrency coverage for PID lifecycle transitions.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use axpoll::PollSet;

use super::*;
use crate::{
    sync::IrqMutex,
    task::{PidReservation, PidReservationKind, Tid},
};

static REAP_CLAIM_BARRIER_PID: AtomicU32 = AtomicU32::new(0);
static REAP_CLAIM_REACHED: AtomicBool = AtomicBool::new(false);
static REAP_CLAIM_RELEASED: AtomicBool = AtomicBool::new(false);

pub(super) fn reap_claim_barrier(tgid: TgidNumber) {
    if REAP_CLAIM_BARRIER_PID.load(Ordering::Acquire) != tgid.get() {
        return;
    }
    REAP_CLAIM_REACHED.store(true, Ordering::Release);
    while !REAP_CLAIM_RELEASED.load(Ordering::Acquire) {
        ax_task::yield_now();
    }
}

pub(crate) fn reaping_identity_is_not_publicly_resolvable_for_test() -> bool {
    let identity = PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::ProcessLeader)
        .unwrap()
        .publish()
        .unwrap();
    let tid_lease = identity.acquire_role::<Tid>().unwrap();
    let tgid_lease = identity.acquire_role::<Tgid>().unwrap();
    let process = Process::new_for_axtest(identity.clone());
    let test_tgid = process.pid();
    identity.mark_task_exited();
    tid_lease.release();
    identity.bind_zombie_for_axtest(
        process.clone(),
        Arc::new(PollSet::new()),
        ZombieSnapshot {
            cred: Arc::new(Cred::default()),
            ptrace_tracer: None,
            is_clone_child: false,
            wait_parent_tid: TidNumber::from(test_tgid.pid_number()),
            cpu_time: ProcessCpuTime::default(),
            tgid_lease,
        },
    );

    REAP_CLAIM_REACHED.store(false, Ordering::Release);
    REAP_CLAIM_RELEASED.store(false, Ordering::Release);
    REAP_CLAIM_BARRIER_PID.store(test_tgid.get(), Ordering::Release);

    let reaped_cpu_time = Arc::new(IrqMutex::new(None));
    let reap_task = {
        let process = process.clone();
        let reaped_cpu_time = reaped_cpu_time.clone();
        ax_task::spawn(move || {
            *reaped_cpu_time.lock() = reap_process(&process);
        })
    };

    while !REAP_CLAIM_REACHED.load(Ordering::Acquire) {
        ax_task::yield_now();
    }
    let number = test_tgid.pid_number();
    let namespace_lookup = ROOT_PID_NS.lookup(number);
    let process_lookup =
        PidView::new(ROOT_PID_NS.clone()).resolve_process(TgidNumber::from(number));
    let identity_process_lookup = identity.public_process();

    REAP_CLAIM_RELEASED.store(true, Ordering::Release);
    reap_task.join();
    REAP_CLAIM_BARRIER_PID.store(0, Ordering::Release);

    let group_and_session_number_retained = namespace_lookup
        .as_ref()
        .is_some_and(|registered| registered.id() == identity.id());
    let view_hidden = matches!(process_lookup, Err(StarryError::NoSuchProcess));
    let identity_hidden = matches!(identity_process_lookup, Err(StarryError::NoSuchProcess));
    let reaped_once = *reaped_cpu_time.lock() == Some(ProcessCpuTime::default());
    group_and_session_number_retained && view_hidden && identity_hidden && reaped_once
}
