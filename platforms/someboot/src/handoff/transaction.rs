//! Exclusive publication ownership, released after platform-owned guards drop.
use core::sync::atomic::{AtomicBool, Ordering};

use super::HandoffError;

static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Keep this as the final field of a prepared handoff so that rollback and
/// buffer destruction finish before another producer can acquire ownership.
pub(crate) struct Transaction(());
impl Transaction {
    pub(crate) fn begin() -> Result<Self, HandoffError> {
        ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| HandoffError::Busy)?;
        Ok(Self(()))
    }
}
impl Drop for Transaction {
    fn drop(&mut self) {
        ACTIVE.store(false, Ordering::Release);
    }
}
