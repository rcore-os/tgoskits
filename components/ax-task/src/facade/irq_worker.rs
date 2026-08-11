use core::time::Duration;

use super::*;

/// Fixed-owner waiter for one hard-IRQ notification cell.
///
/// The IRQ registration and the scheduler park transaction are the only two
/// ownership edges. The waiter deliberately does not enqueue the same thread
/// in a second task wait queue: a direct IRQ wake already targets the park
/// generation owned by the scheduler.
#[derive(Debug)]
pub struct IrqWorkerWaiter {
    registration: IrqWaitRegistration,
}

impl IrqWorkerWaiter {
    /// Binds a reusable IRQ registration to one fixed scheduler thread.
    pub fn new(wake_owner: ThreadWakeHandle) -> Self {
        Self {
            registration: IrqWaitRegistration::new(wake_owner),
        }
    }

    /// Waits until the cell consumes one pending or concurrent notification.
    ///
    /// Unrelated scheduler wakes are retried without republishing the IRQ
    /// registration. This is the same fixed-waiter ownership used by Linux
    /// completion workers: the producer wakes the scheduler task directly and
    /// the scheduler remains the sole owner of its blocked/runnable state.
    pub fn wait(&self, event: &IrqWaitCell) -> Result<(), TaskError> {
        match event.register(&self.registration) {
            IrqRegisterResult::Occupied => Err(TaskError::InvalidConfiguration),
            IrqRegisterResult::ConsumedPending => Ok(()),
            IrqRegisterResult::Registered(token)
            | IrqRegisterResult::NotificationInFlight(token) => loop {
                if !token.is_attached() {
                    return quiesce_irq_wait(token);
                }
                match begin_current_park()? {
                    CurrentParkStart::Notified => {}
                    CurrentParkStart::Prepared(park) => {
                        let _resume = park.commit()?;
                    }
                }
            },
        }
    }

    /// Waits until notification or a relative timeout expires.
    ///
    /// The timeout path retains the generic wait-queue race arbitration until
    /// the IRQ registration exposes a typed notify-versus-timeout outcome.
    /// The unbounded completion hot path uses [`Self::wait`] and owns no
    /// detached task wait queue.
    pub fn wait_timeout(&self, event: &IrqWaitCell, timeout: Duration) -> Result<bool, TaskError> {
        match event.register(&self.registration) {
            IrqRegisterResult::Occupied => Err(TaskError::InvalidConfiguration),
            IrqRegisterResult::ConsumedPending => Ok(false),
            IrqRegisterResult::Registered(token)
            | IrqRegisterResult::NotificationInFlight(token) => {
                let park = WaitQueue::new();
                let timed_out = park.wait_timeout_until(timeout, || !token.is_attached());
                quiesce_irq_wait(token)?;
                Ok(timed_out)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use core::mem::size_of;

    use super::*;

    #[test]
    fn fixed_irq_waiter_has_no_detached_task_wait_queue() {
        assert_eq!(
            size_of::<IrqWorkerWaiter>(),
            size_of::<IrqWaitRegistration>(),
            "a fixed IRQ waiter must use the scheduler park transaction as its only task-wait \
             state"
        );
    }
}
