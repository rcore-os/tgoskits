//! Ticket lock coupled to the runtime's task-preemption service.

use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use super::{RawTicketGuard, RawTicketLock};
use crate::runtime::{PreemptGuardToken, task_runtime};

/// A private ticket lock for scheduler state that hard IRQs never acquire.
#[derive(Debug)]
pub(crate) struct PreemptTicketLock<T> {
    raw: RawTicketLock<T>,
}

impl<T> PreemptTicketLock<T> {
    /// Creates an unlocked task-context ticket lock.
    pub(crate) const fn new(value: T) -> Self {
        Self {
            raw: RawTicketLock::new(value),
        }
    }

    /// Prevents task migration and acquires the cross-CPU ticket lock.
    pub(crate) fn lock(&self) -> PreemptTicketGuard<'_, T> {
        let token = task_runtime::preempt_guard_enter();
        let raw = self.raw.lock();
        PreemptTicketGuard {
            raw: Some(raw),
            token,
            _not_send: PhantomData,
        }
    }

    /// Attempts acquisition and restores preemption state on failure.
    pub(crate) fn try_lock(&self) -> Option<PreemptTicketGuard<'_, T>> {
        let token = task_runtime::preempt_guard_enter();
        match self.raw.try_lock() {
            Some(raw) => Some(PreemptTicketGuard {
                raw: Some(raw),
                token,
                _not_send: PhantomData,
            }),
            None => {
                // SAFETY: this consumes the token just returned on the same
                // task context; no lock guard escaped the failed acquisition.
                unsafe { task_runtime::preempt_guard_exit(token) };
                None
            }
        }
    }
}

/// Preemption-disabled access to a task-only scheduler object.
pub(crate) struct PreemptTicketGuard<'a, T> {
    raw: Option<RawTicketGuard<'a, T>>,
    token: PreemptGuardToken,
    _not_send: PhantomData<*mut ()>,
}

impl<T> Deref for PreemptTicketGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.raw
            .as_deref()
            .expect("preempt ticket guard always owns its raw guard")
    }
}

impl<T> DerefMut for PreemptTicketGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.raw
            .as_deref_mut()
            .expect("preempt ticket guard always owns its raw guard")
    }
}

impl<T> Drop for PreemptTicketGuard<'_, T> {
    fn drop(&mut self) {
        // Publish the protected state before the final preemption exit can
        // enter the scheduler and expose it to another CPU.
        drop(self.raw.take());
        // SAFETY: construction received this token on the current task
        // context, the !Send marker prevents migration, and Drop consumes it
        // exactly once. The runtime accepts non-LIFO nested exits.
        unsafe { task_runtime::preempt_guard_exit(self.token) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn try_lock_failure_restores_its_preempt_nesting() {
        crate::test_runtime::reset_preempt_state();
        let lock = PreemptTicketLock::new(());
        let first = lock.lock();
        assert_eq!(crate::test_runtime::active_preempt_guards(), 1);
        assert!(lock.try_lock().is_none());
        assert_eq!(crate::test_runtime::active_preempt_guards(), 1);
        drop(first);
        assert_eq!(crate::test_runtime::active_preempt_guards(), 0);
    }

    #[test]
    fn non_lifo_guard_drop_keeps_preemption_disabled_until_the_last_guard() {
        crate::test_runtime::reset_preempt_state();
        let first = PreemptTicketLock::new(());
        let second = PreemptTicketLock::new(());
        let first_guard = first.lock();
        let second_guard = second.lock();
        drop(first_guard);
        assert_eq!(crate::test_runtime::active_preempt_guards(), 1);
        drop(second_guard);
        assert_eq!(crate::test_runtime::active_preempt_guards(), 0);
    }
}
