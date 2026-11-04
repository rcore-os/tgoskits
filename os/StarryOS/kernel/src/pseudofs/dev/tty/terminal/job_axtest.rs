//! Axtests for the locking context of the tty foreground job-control check.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use ax_std::os::arceos::task::thread::ThreadState;
use axtest::{AxTestResult, ax_assert, ax_assert_eq};

use super::job::JobControl;
use crate::task::{
    PidReservation, PidReservationKind, Process, ROOT_PID_NS, Tgid, Thread, Tid, UserTaskRef,
    UserThreadOptions, prepare_user_thread, process_group_probe, yield_now,
};

/// Phase handshake between the test thread and the reader thread.
const READER_DONE_UNCONTENDED: u8 = 1;
const GROUP_LOCK_HELD: u8 = 2;

/// Result of one foreground check observed by the reader thread.
#[derive(Default)]
struct ObservedCheck {
    in_foreground: AtomicBool,
    group_lock_acquisitions: AtomicUsize,
    group_lock_acquisitions_with_irqs_disabled: AtomicUsize,
}

/// Prepares a real Starry user thread object with its own process and returns
/// it with the reservation and the process, whose group lock the test can
/// contend. The thread is not schedulable until spawned below.
fn prepare_foreground_reader() -> (Thread, PidReservation, Arc<Process>) {
    let reservation =
        PidReservation::reserve(&ROOT_PID_NS, PidReservationKind::ProcessLeader).unwrap();
    let identity = reservation.identity();
    let tid = identity.acquire_role::<Tid>().unwrap();
    let process_data = crate::task::new_test_process_data(
        identity.clone(),
        identity.acquire_role::<Tgid>().unwrap(),
    );
    let process = process_data.proc.clone();
    crate::mm::copy_from_kernel(&mut process_data.pin_aspace().unwrap().lock()).unwrap();
    let thread = Thread::new(
        identity,
        tid,
        process_data,
        None,
        Default::default(),
        scope_local::Scope::new(),
    )
    .unwrap();
    (thread, reservation, process)
}

/// Makes the prepared thread schedulable; its closure runs as a real Starry
/// user task, so `current_user_task` resolves to the fixture's process.
fn spawn_foreground_reader(
    name: &str,
    thread: Thread,
    body: impl FnOnce() + Send + 'static,
) -> UserTaskRef {
    prepare_user_thread(body, thread, UserThreadOptions::new(name).unwrap())
        .unwrap()
        .stage()
        .unwrap()
        .activate()
}

/// Runs one real foreground check on the calling thread under an armed
/// process-group probe and records what the probe observed.
fn record_check_for_test(observed: &ObservedCheck, job_control: &JobControl) {
    let probe = process_group_probe::arm_for_test();
    let in_foreground = job_control.current_in_foreground();
    let (acquisitions, acquisitions_with_irqs_disabled) = probe.finish_for_test();
    observed.in_foreground.store(in_foreground, Ordering::Release);
    observed
        .group_lock_acquisitions
        .store(acquisitions, Ordering::Release);
    observed
        .group_lock_acquisitions_with_irqs_disabled
        .store(acquisitions_with_irqs_disabled, Ordering::Release);
}

/// Waits until the reader publishes `phase`, bounded by a shared deadline.
fn wait_for_phase_for_test(phase: &AtomicU8, target: u8) -> bool {
    let deadline = ax_runtime::hal::time::monotonic_time_nanos() + 2_000_000_000;
    while phase.load(Ordering::Acquire) != target {
        if ax_runtime::hal::time::monotonic_time_nanos() >= deadline {
            return false;
        }
        yield_now();
    }
    true
}

/// The foreground check must take the sleeping process-group lock in a
/// sleepable context, both without contention and while another thread holds
/// that lock, as a concurrent `setpgid` transaction would.
///
/// Determinism: the observation point in `Process::group` records the
/// interrupt context of the acquisition, so the uncontended phase detects the
/// hazard without waiting for the PI slow path to panic. The contended phase
/// holds the group lock before releasing the reader and only opens it after
/// the reader has parked on it, so the blocking half of the check is forced
/// rather than raced. No scheduling, barriers, or repetition are involved.
#[axtest::axtest]
fn foreground_check_takes_the_group_lock_in_a_sleepable_context() -> AxTestResult {
    let (thread, reservation, process) = prepare_foreground_reader();

    // The reader's process keeps the group it was created with, and the
    // job-control state publishes exactly that group as the foreground one
    // before the reader becomes schedulable, so every observed check must
    // report the reader as foreground.
    let job_control = Arc::new(JobControl::new());
    let own_group = process.group();
    let session = own_group.session();
    job_control.set_session(&session).unwrap();
    job_control.set_foreground(&own_group).unwrap();

    let phase = Arc::new(AtomicU8::new(0));
    let uncontended = Arc::new(ObservedCheck::default());
    let contended = Arc::new(ObservedCheck::default());
    let reader = {
        let job_control = job_control.clone();
        let phase = phase.clone();
        let uncontended = uncontended.clone();
        let contended = contended.clone();
        move || {
            record_check_for_test(&uncontended, &job_control);
            phase.store(READER_DONE_UNCONTENDED, Ordering::Release);

            // The test thread holds the process-group lock from now on; the
            // check below must block on that sleeping lock in a scheduler-safe
            // context until the lock opens.
            while phase.load(Ordering::Acquire) != GROUP_LOCK_HELD {
                yield_now();
            }
            record_check_for_test(&contended, &job_control);
        }
    };
    let task = spawn_foreground_reader("tty-foreground-check", thread, reader);

    // Uncontended phase: the check must read the process group with local
    // IRQs enabled.
    ax_assert!(
        wait_for_phase_for_test(&phase, READER_DONE_UNCONTENDED),
        "the foreground reader never finished the uncontended check"
    );
    ax_assert!(
        uncontended.in_foreground.load(Ordering::Acquire),
        "the reader must be reported in its foreground group"
    );
    ax_assert!(
        uncontended.group_lock_acquisitions.load(Ordering::Acquire) >= 1,
        "the foreground check did not read the process group"
    );
    ax_assert_eq!(
        uncontended
            .group_lock_acquisitions_with_irqs_disabled
            .load(Ordering::Acquire),
        0,
        "the foreground check took the sleeping process-group lock with local IRQs disabled"
    );

    // Contended phase: hold the group lock, as a concurrent setpgid
    // transaction would, and only open it once the reader has parked on it.
    let group_lock = process.group.lock();
    phase.store(GROUP_LOCK_HELD, Ordering::Release);
    let deadline = ax_runtime::hal::time::monotonic_time_nanos() + 2_000_000_000;
    while task.state() != ThreadState::Blocked {
        ax_assert!(
            ax_runtime::hal::time::monotonic_time_nanos() < deadline,
            "the foreground reader never blocked on the process-group lock"
        );
        yield_now();
    }
    drop(group_lock);

    ax_assert_eq!(
        task.join(),
        0,
        "the foreground reader must finish once the process-group lock opens"
    );
    ax_assert!(
        contended.in_foreground.load(Ordering::Acquire),
        "the contended reader must be reported in its foreground group"
    );
    ax_assert!(
        contended.group_lock_acquisitions.load(Ordering::Acquire) >= 1,
        "the contended foreground check did not read the process group"
    );
    ax_assert_eq!(
        contended
            .group_lock_acquisitions_with_irqs_disabled
            .load(Ordering::Acquire),
        0,
        "the contended foreground check took the sleeping process-group lock with local IRQs disabled"
    );

    drop(reservation);
    AxTestResult::Ok
}
