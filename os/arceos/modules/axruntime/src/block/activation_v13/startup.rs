//! One-shot publication from a pinned maintenance owner to its activator.

use ax_kspin::SpinNoPreempt;

use crate::task::{TaskError, WaitQueue};

pub(super) struct OwnerStartupCell<T> {
    value: SpinNoPreempt<Option<T>>,
    wait: WaitQueue,
}

/// One-shot task-context handoff used before a non-migratable owner starts.
pub(super) struct OwnerTransferCell<T> {
    value: SpinNoPreempt<Option<T>>,
}

impl<T> OwnerTransferCell<T> {
    pub(super) const fn new(value: T) -> Self {
        Self {
            value: SpinNoPreempt::new(Some(value)),
        }
    }

    pub(super) fn take(&self) -> Option<T> {
        self.value.lock().take()
    }
}

impl<T> OwnerStartupCell<T> {
    pub(super) const fn new() -> Self {
        Self {
            value: SpinNoPreempt::new(None),
            wait: WaitQueue::new(),
        }
    }

    pub(super) fn publish(&self, value: T) -> Result<(), T> {
        let mut slot = self.value.lock();
        if slot.is_some() {
            return Err(value);
        }
        *slot = Some(value);
        drop(slot);
        self.wait.notify_all();
        Ok(())
    }

    pub(super) fn wait_take(&self) -> Result<T, TaskError> {
        self.wait.try_wait_until(|| self.value.lock().is_some())?;
        Ok(self
            .value
            .lock()
            .take()
            .expect("the startup value was observed before its unique take"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_publication_returns_the_unaccepted_owner() {
        let slot = OwnerStartupCell::new();

        assert_eq!(slot.publish(3_u8), Ok(()));
        assert_eq!(slot.publish(7_u8), Err(7));
    }
}
