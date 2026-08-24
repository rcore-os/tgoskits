//! Test-only synchronization for the faultable user-copy QEMU regression.

use core::sync::atomic::{AtomicU8, Ordering};

use crate::task::{try_current_user_task, yield_now};

const IDLE: u8 = 0;
const ADDRESS_SPACE_HELD: u8 = 1;
const USER_COPY_COMPLETED: u8 = 2;
const EAGER_PREPARATION_OBSERVED: u8 = 3;

static USER_COPY_TEST_STATE: AtomicU8 = AtomicU8::new(IDLE);

/// Holds the calling process' address-space lock until a concurrent ordinary
/// user copy either completes or attempts eager address-space preparation.
pub(crate) fn hold_address_space_until_user_copy() -> bool {
    let Ok(Some(task)) = try_current_user_task() else {
        return false;
    };
    let address_space = task.as_thread().proc_data.aspace();
    let _guard = address_space.lock();
    USER_COPY_TEST_STATE.store(ADDRESS_SPACE_HELD, Ordering::Release);

    loop {
        match USER_COPY_TEST_STATE.load(Ordering::Acquire) {
            ADDRESS_SPACE_HELD => yield_now(),
            USER_COPY_COMPLETED | EAGER_PREPARATION_OBSERVED => return true,
            _ => return false,
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
pub(crate) fn record_eager_user_memory_preparation() {
    let _ = USER_COPY_TEST_STATE.compare_exchange(
        ADDRESS_SPACE_HELD,
        EAGER_PREPARATION_OBSERVED,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}

/// Records a successful ordinary copy while the regression holds the process'
/// address-space lock on another thread.
pub(crate) fn record_user_copy_completed() {
    let _ = USER_COPY_TEST_STATE.compare_exchange(
        ADDRESS_SPACE_HELD,
        USER_COPY_COMPLETED,
        Ordering::AcqRel,
        Ordering::Acquire,
    );
}
