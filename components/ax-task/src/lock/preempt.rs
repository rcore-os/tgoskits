//! Ticket lock coupled to the runtime's task-preemption service.

use core::{
    marker::PhantomData,
    ops::{Deref, DerefMut},
};

use super::{RawTicketGuard, RawTicketLock};
use crate::runtime::{PreemptGuardToken, enter_preempt_guard, task_runtime};

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
        let scope = PreemptScope::enter();
        let raw = self.raw.lock();
        PreemptTicketGuard {
            raw: Some(raw),
            scope: Some(scope),
            _not_send: PhantomData,
        }
    }

    /// Attempts acquisition and restores preemption state on failure.
    #[cfg(test)]
    pub(crate) fn try_lock(&self) -> Option<PreemptTicketGuard<'_, T>> {
        let scope = PreemptScope::enter();
        match self.raw.try_lock() {
            Some(raw) => Some(PreemptTicketGuard {
                raw: Some(raw),
                scope: Some(scope),
                _not_send: PhantomData,
            }),
            None => None,
        }
    }
}

/// Task-preemption exclusion owned by one complete scheduler transaction.
///
/// Unlike a lock guard, this scope can remain live after metadata locks are
/// released, for example while an rtmutex-style wake queue is published. A
/// stronger IRQ or scheduler owner scope is represented by a runtime `NONE`
/// token and therefore needs no synthetic exit.
pub(crate) struct PreemptScope {
    token: PreemptGuardToken,
    _not_send: PhantomData<*mut ()>,
}

impl PreemptScope {
    pub(crate) fn enter() -> Self {
        Self {
            token: enter_preempt_guard(),
            _not_send: PhantomData,
        }
    }
}

impl Drop for PreemptScope {
    fn drop(&mut self) {
        if self.token.is_none() {
            return;
        }
        // SAFETY: this !Send scope consumes the token returned on the same
        // task context after every protected publication is complete.
        unsafe { task_runtime::preempt_guard_exit(self.token) };
    }
}

/// Preemption-disabled access to a task-only scheduler object.
pub(crate) struct PreemptTicketGuard<'a, T> {
    raw: Option<RawTicketGuard<'a, T>>,
    scope: Option<PreemptScope>,
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
        drop(self.scope.take());
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
