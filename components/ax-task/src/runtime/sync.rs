//! Scheduler-owned synchronization provider capabilities.

pub use crate::{
    runtime::sync::pi::{
        pi_drop_wait_handle, pi_initial_owner_is_on_cpu, pi_mutex_claim, pi_mutex_lock_slow,
        pi_mutex_release_owned, pi_park_current_once, pi_wait_cancel, pi_wait_try_cancel,
        pi_waiter_is_granted, pi_waiter_is_top,
    },
    sync::{
        bridge::*,
        mutex::{
            PI_MUTEX_WAIT_STORAGE_WORDS, PiMutexAcquire, PiMutexClaimOutcome, PiMutexCore,
            PiMutexCoreView, PiMutexId, PiMutexLockResult, PiMutexOwnedRelease,
            PiMutexOwnerSnapshot, PiMutexRaw, PiMutexRef, PiMutexStateError, PiTaskId,
            PiWaitCancelOutcome, PiWaitToken,
        },
    },
    thread::error::PiWaitStateError,
};

pub(crate) mod pi;

pub(crate) mod rt_lock;
