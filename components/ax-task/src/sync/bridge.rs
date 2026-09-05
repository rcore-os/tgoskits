//! Scheduler capabilities consumed by the runtime lock provider.
//!
//! This module is public only because the provider lives in `ax-runtime`.
//! Lock algorithms and OS consumers must use [`super::api`] instead. These
//! exports retain the task scheduler as the sole owner of PI waiter nodes,
//! donation chains, and park/wake handshakes. Lockdep state is consumed
//! directly by the task-owned algorithms and is not a provider surface.
mod context;
mod lockdep;
mod mutex;
mod spin;

pub use self::{
    context::{
        ContextOperations, ContextState, context_enter, context_exit, hardirq_enter, hardirq_exit,
        irq_return_preempt_enter, irq_return_preempt_exit,
    },
    lockdep::{LockClass, dump_lockdep_trace, set_lockdep_trace_enabled},
    mutex::{
        MutexAcquireRequest, PiMutexStorage, PiMutexStorageMut, mutex_acquire, mutex_destroy,
        mutex_force_release, mutex_is_locked, mutex_is_owned_by_current, mutex_release,
        mutex_try_acquire,
    },
    spin::{
        RwLockAcquireRequest, SpinAcquireRequest, rwlock_acquire, rwlock_force_read_decrement,
        rwlock_release, rwlock_try_acquire, spin_acquire, spin_force_release, spin_is_locked,
        spin_release, spin_try_acquire,
    },
};
