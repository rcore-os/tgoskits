use alloc::{sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicUsize, Ordering};

use ax_errno::AxResult;

use crate::os::{BlockNotification, runtime_ops, sync::IrqMutex};

/// Task-context waiters whose wakeups must not be coalesced with each other.
///
/// Each blocked task owns an independent notification. State owners publish
/// their state transition first and then wake the registered tasks. Registering
/// before rechecking the predicate closes the transition-to-sleep race.
pub(super) struct TaskWaiters {
    notifications: IrqMutex<Vec<Arc<dyn BlockNotification>>>,
}

impl TaskWaiters {
    pub(super) const fn new() -> Self {
        Self {
            notifications: IrqMutex::new(Vec::new()),
        }
    }

    /// Registers the current task and sleeps when `should_wait` remains true.
    ///
    /// This function is task-context only. `should_wait` must only observe the
    /// state whose publisher calls [`notify_all`](Self::notify_all).
    pub(super) fn wait_while(&self, should_wait: impl FnOnce() -> bool) -> AxResult {
        let notification = runtime_ops()?.notification();
        self.notifications.lock().push(Arc::clone(&notification));

        if should_wait() {
            notification.wait();
        }
        self.remove(&notification);
        Ok(())
    }

    /// Wakes one registered task.
    pub(super) fn notify_one(&self) {
        let notification = {
            let mut notifications = self.notifications.lock();
            if notifications.is_empty() {
                None
            } else {
                Some(notifications.remove(0))
            }
        };
        if let Some(notification) = notification {
            notification.notify();
        }
    }

    /// Wakes every task registered before the associated state publication.
    pub(super) fn notify_all(&self) {
        let notifications = core::mem::take(&mut *self.notifications.lock());
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
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.notifications.lock().len()
    }
}

struct CapacityWaiter {
    required: usize,
    notification: Arc<dyn BlockNotification>,
}

/// Task waiters blocked on bounded-channel capacity.
///
/// Unlike a broadcast wait set, this registry wakes only a set of producers
/// whose requests can fit in the newly available capacity. A producer that
/// still cannot fit hands unused capacity to smaller waiters before sleeping.
pub(super) struct CapacityWaiters {
    waiters: IrqMutex<Vec<CapacityWaiter>>,
    count: AtomicUsize,
}

impl CapacityWaiters {
    pub(super) const fn new() -> Self {
        Self {
            waiters: IrqMutex::new(Vec::new()),
            count: AtomicUsize::new(0),
        }
    }

    pub(super) fn wait_for(&self, required: usize, available: impl FnOnce() -> usize) -> AxResult {
        let notification = runtime_ops()?.notification();
        {
            let mut waiters = self.waiters.lock();
            waiters.push(CapacityWaiter {
                required,
                notification: Arc::clone(&notification),
            });
            self.count.store(waiters.len(), Ordering::Release);
        }

        let available = available();
        if available >= required {
            self.remove(&notification);
        } else {
            // This waiter cannot use a partial gap, but a smaller waiter may.
            self.notify_available(available);
            notification.wait();
            self.remove(&notification);
        }
        Ok(())
    }

    pub(super) fn notify_available(&self, mut available: usize) {
        if available == 0 || self.count.load(Ordering::Acquire) == 0 {
            return;
        }

        let notifications = {
            let mut waiters = self.waiters.lock();
            let mut notifications = Vec::new();
            let mut index = 0;
            while index < waiters.len() && available != 0 {
                if waiters[index].required <= available {
                    let waiter = waiters.remove(index);
                    available -= waiter.required;
                    notifications.push(waiter.notification);
                } else {
                    index += 1;
                }
            }
            self.count.store(waiters.len(), Ordering::Release);
            notifications
        };
        for notification in notifications {
            notification.notify();
        }
    }

    pub(super) fn notify_all(&self) {
        let waiters = core::mem::take(&mut *self.waiters.lock());
        self.count.store(0, Ordering::Release);
        for waiter in waiters {
            waiter.notification.notify();
        }
    }

    fn remove(&self, notification: &Arc<dyn BlockNotification>) {
        let mut waiters = self.waiters.lock();
        if let Some(index) = waiters
            .iter()
            .position(|waiter| Arc::ptr_eq(&waiter.notification, notification))
        {
            waiters.remove(index);
            self.count.store(waiters.len(), Ordering::Release);
        }
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.count.load(Ordering::Acquire)
    }
}
