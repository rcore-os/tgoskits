//! Native ArceOS lock facade and `ax-sync` bridge provider.

#[cfg(feature = "multitask")]
use core::sync::atomic::{AtomicPtr, AtomicU64};
use core::{
    panic::Location,
    sync::atomic::{AtomicBool, AtomicUsize},
};

pub use ax_task::sync::api::*;

struct RuntimeContextOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::ContextOps for RuntimeContextOps {
    fn enter(context: u8) -> usize {
        ax_task::sync::bridge::context_enter(context)
    }

    fn exit(context: u8, state: usize) {
        ax_task::sync::bridge::context_exit(context, state);
    }

    fn exit_preempt_from_irq_return(state: usize) {
        ax_task::sync::bridge::preempt_exit_from_irq_return(state);
    }
}

struct RuntimeSpinOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::SpinOps for RuntimeSpinOps {
    fn acquire(
        locked: &AtomicBool,
        metadata: &ax_sync::interface::LockMetadata,
        lock_addr: usize,
        context: u8,
        subclass: u32,
        is_try: bool,
        caller: &'static Location<'static>,
    ) -> ax_sync::interface::AcquireResult {
        let (acquired, context_state) =
            ax_task::sync::bridge::spin_acquire(ax_task::sync::bridge::SpinAcquireRequest {
                locked,
                class: ax_task::sync::bridge::LockClass {
                    class_id: metadata.class_id(),
                    class_key: metadata.class_key(),
                },
                lock_addr,
                context,
                subclass,
                is_try,
                caller,
            });
        ax_sync::interface::AcquireResult::new(acquired, context_state)
    }

    fn release(locked: &AtomicBool, lock_addr: usize, context: u8, context_state: usize) {
        ax_task::sync::bridge::spin_release(locked, lock_addr, context, context_state);
    }

    fn force_release(locked: &AtomicBool, lock_addr: usize, context: u8) {
        ax_task::sync::bridge::spin_force_release(locked, lock_addr, context);
    }

    fn is_locked(locked: &AtomicBool) -> bool {
        ax_task::sync::bridge::spin_is_locked(locked)
    }
}

struct RuntimeRwLockOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::RwLockOps for RuntimeRwLockOps {
    fn acquire(
        state: &AtomicUsize,
        metadata: &ax_sync::interface::LockMetadata,
        lock_addr: usize,
        context: u8,
        mode: u8,
        is_try: bool,
        caller: &'static Location<'static>,
    ) -> ax_sync::interface::AcquireResult {
        let (acquired, context_state) =
            ax_task::sync::bridge::rwlock_acquire(ax_task::sync::bridge::RwLockAcquireRequest {
                state,
                class: ax_task::sync::bridge::LockClass {
                    class_id: metadata.class_id(),
                    class_key: metadata.class_key(),
                },
                lock_addr,
                context,
                mode,
                is_try,
                caller,
            });
        ax_sync::interface::AcquireResult::new(acquired, context_state)
    }

    fn release(state: &AtomicUsize, lock_addr: usize, context: u8, context_state: usize, mode: u8) {
        ax_task::sync::bridge::rwlock_release(state, lock_addr, context, context_state, mode);
    }

    fn force_read_decrement(state: &AtomicUsize, lock_addr: usize, context: u8) {
        ax_task::sync::bridge::rwlock_force_read_decrement(state, lock_addr, context);
    }
}

#[cfg(feature = "multitask")]
struct RuntimeMutexOps;

#[cfg(feature = "multitask")]
#[ax_crate_interface::impl_interface]
impl ax_sync::interface::MutexOps for RuntimeMutexOps {
    fn acquire(
        wait_queue: &AtomicPtr<()>,
        owner_id: &AtomicU64,
        metadata: &ax_sync::interface::LockMetadata,
        lock_addr: usize,
        subclass: u32,
        is_try: bool,
        caller: &'static Location<'static>,
    ) -> bool {
        ax_task::sync::bridge::mutex_acquire(ax_task::sync::bridge::MutexAcquireRequest {
            wait_queue,
            owner_id,
            class: ax_task::sync::bridge::LockClass {
                class_id: metadata.class_id(),
                class_key: metadata.class_key(),
            },
            lock_addr,
            subclass,
            is_try,
            caller,
        })
    }

    fn release(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, lock_addr: usize) {
        ax_task::sync::bridge::mutex_release(wait_queue, owner_id, lock_addr);
    }

    fn force_release(wait_queue: &AtomicPtr<()>, owner_id: &AtomicU64, lock_addr: usize) {
        ax_task::sync::bridge::mutex_force_release(wait_queue, owner_id, lock_addr);
    }

    fn is_owned_by_current(owner_id: &AtomicU64) -> bool {
        ax_task::sync::bridge::mutex_is_owned_by_current(owner_id)
    }

    fn is_locked(owner_id: &AtomicU64) -> bool {
        ax_task::sync::bridge::mutex_is_locked(owner_id)
    }

    fn drop_wait_queue(wait_queue: *mut ()) {
        ax_task::sync::bridge::mutex_drop_wait_queue(wait_queue);
    }
}

struct RuntimeLockdepOps;

#[ax_crate_interface::impl_interface]
impl ax_sync::interface::LockdepOps for RuntimeLockdepOps {
    fn set_trace_enabled(enabled: bool) {
        ax_task::sync::bridge::set_lockdep_trace_enabled(enabled);
    }

    fn dump_trace() {
        ax_task::sync::bridge::dump_lockdep_trace();
    }
}
