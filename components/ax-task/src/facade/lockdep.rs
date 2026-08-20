//! Current-task access to task-owned lockdep state.

use crate::sync::{
    IrqSaveGuard,
    lockdep::{HeldLock, HeldLockSnapshot, HeldLockStack},
};

fn with_current_held_locks(operation: impl FnOnce(&mut HeldLockStack)) {
    let _irq_guard = IrqSaveGuard::new();
    let Ok(current) = super::current_thread_handle() else {
        return;
    };
    // SAFETY: local IRQ exclusion prevents migration and scheduler replacement
    // of `current` until the complete task-owned stack operation has finished.
    unsafe { current.core.with_held_locks(operation) };
}

/// Copies the calling task's held-lock stack into a lockdep snapshot.
#[doc(hidden)]
pub(crate) fn collect_current_task_held_locks(snapshot: &mut HeldLockSnapshot) {
    with_current_held_locks(|stack| snapshot.extend(stack));
}

/// Records one lock acquisition in the calling task's lockdep state.
#[doc(hidden)]
pub(crate) fn push_current_task_held_lock(held: HeldLock) {
    with_current_held_locks(|stack| stack.push(held));
}

/// Records one lock release in the calling task's lockdep state.
#[doc(hidden)]
pub(crate) fn pop_current_task_held_lock(lock_addr: usize) {
    with_current_held_locks(|stack| stack.pop_checked(lock_addr));
}
