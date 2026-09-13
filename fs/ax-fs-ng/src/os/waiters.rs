use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::{
    BlockError, BlockResult,
    os::{BlockNotification, runtime_ops, sync::IrqMutex},
};

/// Task-context waiters whose wakeups must not be coalesced with each other.
///
/// Each blocked task owns an independent notification. State owners publish
/// their state transition first and then wake the registered tasks. Registering
/// before rechecking the predicate closes the transition-to-sleep race.
pub(crate) struct TaskWaiters {
    notifications: IrqMutex<Vec<Arc<dyn BlockNotification>>>,
    count: AtomicUsize,
}

impl TaskWaiters {
    pub(crate) const fn new() -> Self {
        Self {
            notifications: IrqMutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        }
    }

    /// Registers the current task and sleeps when `should_wait` remains true.
    ///
    /// This function is task-context only. `should_wait` must only observe the
    /// state whose publisher calls [`notify_all`](Self::notify_all).
    pub(crate) fn wait_while(&self, should_wait: impl FnOnce() -> bool) -> BlockResult {
        let runtime = runtime_ops()?;
        if !runtime.can_block() {
            return Err(BlockError::WouldBlock);
        }
        let notification = runtime.notification();
        self.register_with(&notification, |spare, required| {
            spare
                .try_reserve_exact(required)
                .map_err(|_| BlockError::NoMemory)
        })?;

        if should_wait() {
            notification.wait();
        }
        self.remove(&notification);
        Ok(())
    }

    fn register_with(
        &self,
        notification: &Arc<dyn BlockNotification>,
        mut reserve: impl FnMut(&mut Vec<Arc<dyn BlockNotification>>, usize) -> BlockResult,
    ) -> BlockResult {
        let mut spare = Vec::new();
        loop {
            let mut notifications = self.notifications.lock();
            if notifications.len() == notifications.capacity() {
                let required = notifications
                    .len()
                    .checked_add(1)
                    .ok_or(BlockError::NoMemory)?;
                if spare.capacity() < required {
                    drop(notifications);
                    reserve(&mut spare, required)?;
                    continue;
                }
                // Allocation and old-buffer release happen outside IRQ
                // exclusion. A concurrent registrar may require another try.
                spare.append(&mut notifications);
                core::mem::swap(&mut spare, &mut notifications);
            }
            notifications.push(Arc::clone(notification));
            self.count.store(notifications.len(), Ordering::Release);
            return Ok(());
        }
    }

    /// Wakes every registered task after the associated state publication.
    pub(crate) fn notify_all(&self) {
        if self.count.load(Ordering::Acquire) == 0 {
            return;
        }
        let notifications = {
            let mut notifications = self.notifications.lock();
            let pending = core::mem::take(&mut *notifications);
            self.count.store(0, Ordering::Release);
            pending
        };
        for notification in notifications {
            notification.notify();
        }
    }

    fn remove(&self, notification: &Arc<dyn BlockNotification>) {
        let mut notifications = self.notifications.lock();
        if let Some(index) = notifications
            .iter()
            .position(|candidate| Arc::ptr_eq(candidate, notification))
        {
            notifications.remove(index);
            self.count.store(notifications.len(), Ordering::Release);
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.notifications.lock().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlockError;

    #[test]
    fn registration_allocation_runs_unlocked_and_failure_adds_no_waiter() {
        crate::os::task::install_test_runtime_ops();
        let waiters = TaskWaiters::new();
        let notification = runtime_ops().unwrap().notification();
        let result = waiters.register_with(&notification, |_, _| {
            assert!(waiters.notifications.try_lock().is_some());
            Err(BlockError::NoMemory)
        });
        assert_eq!(result, Err(BlockError::NoMemory));
        assert_eq!(waiters.len(), 0);
        assert_eq!(waiters.count.load(Ordering::Acquire), 0);
    }

    #[test]
    fn notification_between_registration_and_predicate_is_not_lost() {
        crate::os::task::install_test_runtime_ops();
        let waiters = TaskWaiters::new();
        waiters
            .wait_while(|| {
                waiters.notify_all();
                true
            })
            .unwrap();
        assert_eq!(waiters.len(), 0);
    }
}
