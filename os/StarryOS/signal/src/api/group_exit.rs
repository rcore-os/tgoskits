//! The single thread-group exit decision shared with process teardown.

use ax_runtime::task::sync::RawSpinLock;

/// Irrevocable Linux `SIGNAL_GROUP_EXIT` decision and its original wait status.
///
/// Signal publication can commit this state while holding its disposition lock.
/// The guard protects only the status: no allocation, task lookup, callback or
/// sleeping lock acquisition takes place here. Process teardown observes the
/// same decision instead of replacing the original signal with a wakeup SIGKILL.
#[derive(Default)]
pub struct GroupExit {
    status: RawSpinLock<Option<i32>>,
}

impl GroupExit {
    /// Publishes the first exit decision; later callers cannot change its code.
    pub fn begin(&self, status: i32) -> bool {
        let mut current = self.status.lock_irqsave();
        if current.is_some() {
            return false;
        }
        *current = Some(status);
        true
    }

    /// Returns the committed Linux wait status, or `None` for a live group.
    pub fn status(&self) -> Option<i32> {
        *self.status.lock_irqsave()
    }
}
