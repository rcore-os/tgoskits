//! Test-only synchronization for the faultable user-copy QEMU regression.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU8, AtomicUsize, Ordering};

use crate::task::{UserTaskRef, try_current_user_task, yield_now};

const IDLE: u8 = 0;
const HOLDER_READY: u8 = 1;
const USER_COPY_READY: u8 = 2;
const ADDRESS_SPACE_HELD: u8 = 3;
const USER_COPY_COMPLETED: u8 = 4;
const EAGER_PREPARATION_OBSERVED: u8 = 5;
const USER_COPY_FAULT_OBSERVED: u8 = 6;

static USER_COPY_TEST_STATE: AtomicU8 = AtomicU8::new(IDLE);
static USER_COPY_TEST_ADDRESS_SPACE: AtomicUsize = AtomicUsize::new(0);

fn address_space_identity(task: &UserTaskRef) -> usize {
    Arc::as_ptr(&task.as_thread().proc_data.aspace()) as usize
}

fn belongs_to_armed_test(task: &UserTaskRef, expected_state: u8) -> bool {
    // Acquiring the state publication also makes the preceding address-space
    // identity publication visible on weakly ordered architectures.
    USER_COPY_TEST_STATE.load(Ordering::Acquire) == expected_state
        && USER_COPY_TEST_ADDRESS_SPACE.load(Ordering::Relaxed) == address_space_identity(task)
}

/// Coordinates an address-space holder with an ordinary user copy.
///
/// The holder does not acquire the lock until the target copy is already in
/// the kernel. This keeps user-mode page faults out of the protected window:
/// Linux permits both user faults and ordinary uaccess faults to acquire
/// `mmap_lock`, so letting userspace poll while its equivalent is held would
/// create an invalid circular wait in the regression itself.
pub(crate) fn hold_address_space_until_user_copy() -> bool {
    let Ok(Some(task)) = try_current_user_task() else {
        return false;
    };
    let address_space = task.as_thread().proc_data.aspace();
    USER_COPY_TEST_ADDRESS_SPACE.store(Arc::as_ptr(&address_space) as usize, Ordering::Release);
    USER_COPY_TEST_STATE.store(HOLDER_READY, Ordering::Release);

    loop {
        match USER_COPY_TEST_STATE.load(Ordering::Acquire) {
            HOLDER_READY => yield_now(),
            USER_COPY_READY => break,
            EAGER_PREPARATION_OBSERVED => return true,
            _ => return false,
        }
    }

    let _guard = address_space.lock();
    if USER_COPY_TEST_STATE
        .compare_exchange(
            USER_COPY_READY,
            ADDRESS_SPACE_HELD,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return false;
    }

    loop {
        match USER_COPY_TEST_STATE.load(Ordering::Acquire) {
            ADDRESS_SPACE_HELD => yield_now(),
            USER_COPY_COMPLETED | EAGER_PREPARATION_OBSERVED | USER_COPY_FAULT_OBSERVED => {
                return true;
            }
            _ => return false,
        }
    }
}

/// Waits at the real user-copy boundary until the test holder owns the
/// address-space lock.
///
/// Calls outside the one armed regression window are no-ops. In particular,
/// the debugfs write that starts the holder performs its own user copy before
/// the holder publishes [`HOLDER_READY`].
pub(crate) fn synchronize_user_copy_with_address_space_holder(task: &UserTaskRef) {
    if !belongs_to_armed_test(task, HOLDER_READY) {
        return;
    }
    if USER_COPY_TEST_STATE
        .compare_exchange(
            HOLDER_READY,
            USER_COPY_READY,
            Ordering::AcqRel,
            Ordering::Acquire,
        )
        .is_err()
    {
        return;
    }

    loop {
        match USER_COPY_TEST_STATE.load(Ordering::Acquire) {
            USER_COPY_READY => yield_now(),
            ADDRESS_SPACE_HELD => return,
            _ => return,
        }
    }
}

/// Returns the state as a file length so userspace can observe it via lseek
/// without performing another user-memory copy.
pub(crate) fn observe_user_copy_test_state() -> usize {
    USER_COPY_TEST_STATE.load(Ordering::Acquire) as usize
}

/// Records the lock-taking preparation that Linux does not perform before an
/// ordinary `copy_to_user` or `copy_from_user` operation.
pub(crate) fn record_eager_user_memory_preparation(task: &UserTaskRef) {
    if !belongs_to_armed_test(task, HOLDER_READY) {
        return;
    }
    let _ = USER_COPY_TEST_STATE.compare_exchange(
        HOLDER_READY,
        EAGER_PREPARATION_OBSERVED,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

/// Records a fault while the resident-copy test holds the address-space lock.
///
/// Linux permits ordinary user copies to fault and acquire `mmap_lock`. Publish
/// this before Starry's kernel-uaccess fault path takes the equivalent
/// address-space lock, so the test holder releases its lock and reports the
/// broken resident-page premise instead of manufacturing a circular wait.
pub(crate) fn record_faulting_user_copy(task: &UserTaskRef) -> bool {
    if !belongs_to_armed_test(task, ADDRESS_SPACE_HELD) {
        return false;
    }
    USER_COPY_TEST_STATE
        .compare_exchange(
        ADDRESS_SPACE_HELD,
        USER_COPY_FAULT_OBSERVED,
        Ordering::AcqRel,
        Ordering::Acquire,
    )
        .is_ok()
}

/// Records a successful ordinary copy while the regression holds the process'
/// address-space lock on another thread.
pub(crate) fn record_user_copy_completed(task: &UserTaskRef) {
    if !belongs_to_armed_test(task, ADDRESS_SPACE_HELD) {
        return;
    }
    let _ = USER_COPY_TEST_STATE.compare_exchange(
        ADDRESS_SPACE_HELD,
        USER_COPY_COMPLETED,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}
