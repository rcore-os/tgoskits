//! Deterministic concurrency coverage for PID lifecycle transitions.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use super::*;
use crate::task::{join_kernel_thread, spawn_kernel_thread, yield_now};

const TEST_PID: Pid = Pid::MAX;

static REAP_CLAIM_BARRIER_PID: AtomicU32 = AtomicU32::new(0);
static REAP_CLAIM_REACHED: AtomicBool = AtomicBool::new(false);
static REAP_CLAIM_RELEASED: AtomicBool = AtomicBool::new(false);

pub(super) fn reap_claim_barrier(pid: Pid) {
    if REAP_CLAIM_BARRIER_PID.load(Ordering::Acquire) != pid {
        return;
    }

    REAP_CLAIM_REACHED.store(true, Ordering::Release);
    while !REAP_CLAIM_RELEASED.load(Ordering::Acquire) {
        yield_now();
    }
}

pub(crate) fn reaping_identity_is_not_publicly_resolvable_for_test() -> bool {
    let process = Process::new_for_axtest(TEST_PID);
    let identity = Arc::new(ProcessIdentity {
        process: process.clone(),
        pid_namespaces: Arc::from([Arc::clone(&axnsproxy::ROOT_PID_NS)]),
        exit_event: Arc::new(PollSet::new()),
        state: SpinNoIrq::new(ProcessIdentityState::Zombie(ZombieSnapshot {
            cred: Arc::new(Cred::default()),
            nice: 0,
            ptrace_tracer_pid: None,
            is_clone_child: false,
            wait_parent_tid: TEST_PID,
            cpu_time: ProcessCpuTime::default(),
        })),
    });
    assert!(
        PROCESS_TABLE.lock().insert(TEST_PID, identity).is_none(),
        "test PID must not already be registered"
    );

    REAP_CLAIM_REACHED.store(false, Ordering::Release);
    REAP_CLAIM_RELEASED.store(false, Ordering::Release);
    REAP_CLAIM_BARRIER_PID.store(TEST_PID, Ordering::Release);

    let reaped_cpu_time = Arc::new(SpinNoIrq::new(None));
    let reap_task = {
        let process = process.clone();
        let reaped_cpu_time = reaped_cpu_time.clone();
        spawn_kernel_thread(
            move || {
                *reaped_cpu_time.lock() = reap_process(&process);
            },
            "pid-reap-race".into(),
        )
    };

    while !REAP_CLAIM_REACHED.load(Ordering::Acquire) {
        yield_now();
    }
    let lookup_result = pidfd_process_identity(TEST_PID);
    let thread_lookup_result = pidfd_thread_identity(&process);
    let process_lookup_result = get_process(TEST_PID);
    let getsid_result = crate::syscall::sys_getsid(TEST_PID);
    let getpgid_result = crate::syscall::sys_getpgid(TEST_PID);

    REAP_CLAIM_RELEASED.store(true, Ordering::Release);
    join_kernel_thread(reap_task);
    REAP_CLAIM_BARRIER_PID.store(0, Ordering::Release);

    matches!(lookup_result, Err(AxError::NoSuchProcess))
        && thread_lookup_result.is_none()
        && matches!(process_lookup_result, Err(AxError::NoSuchProcess))
        && matches!(getsid_result, Err(AxError::NoSuchProcess))
        && matches!(getpgid_result, Err(AxError::NoSuchProcess))
        && *reaped_cpu_time.lock() == Some(ProcessCpuTime::default())
        && !PROCESS_TABLE.lock().contains_key(&TEST_PID)
}
