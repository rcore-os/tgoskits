//! Scheduler capabilities consumed by the runtime lock provider.
//!
//! This module is public only because the provider lives in `ax-runtime`.
//! Lock algorithms and OS consumers must use [`super::api`] instead. These
//! exports retain the task scheduler as the sole owner of PI waiter nodes,
//! donation chains, park/wake handshakes, and task-local lockdep state.

#[cfg(feature = "lockdep")]
pub use crate::{
    collect_current_task_held_locks, pop_current_task_held_lock, push_current_task_held_lock,
};
pub use crate::{
    current_needs_reschedule_pinned, current_thread_id, current_thread_token, pi_drop_wait_handle,
    pi_initial_owner_is_on_cpu, pi_mutex_claim, pi_mutex_lock_slow, pi_mutex_release_owned,
    pi_park_current_once, pi_wait_try_cancel, pi_waiter_is_granted, pi_waiter_is_top,
    validate_blocking_context,
};
