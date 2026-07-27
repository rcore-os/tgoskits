//! Binding between a Linux thread identity and its scheduler registry record.

use core::sync::atomic::{AtomicU64, Ordering};

use ax_errno::{AxError, AxResult};
use ax_std::os::arceos::task::ThreadId;

/// A scheduler identity that may be published exactly once.
///
/// The contained [`ThreadId`] includes the registry generation. Keeping it as
/// an opaque value prevents Linux TIDs from being used to reconstruct scheduler
/// identities after a registry slot has been reused.
pub(super) struct SchedulerIdentity {
    id: AtomicU64,
}

impl SchedulerIdentity {
    /// Creates an identity slot for a not-yet-published scheduler thread.
    pub(super) const fn unbound() -> Self {
        Self {
            id: AtomicU64::new(0),
        }
    }

    /// Returns the bound generation-bearing identity.
    pub(super) fn get(&self) -> Option<ThreadId> {
        decode(self.id.load(Ordering::Acquire))
    }

    /// Publishes the identity returned by scheduler thread creation.
    ///
    /// Repeating the same publication is harmless. A different identity means
    /// one Starry thread object was attached to two scheduler records.
    pub(super) fn bind(&self, id: ThreadId) -> AxResult<()> {
        let raw = id.as_u64();
        debug_assert_ne!(raw, 0, "a published scheduler identity cannot be zero");
        match self
            .id
            .compare_exchange(0, raw, Ordering::Release, Ordering::Acquire)
        {
            Ok(_) => Ok(()),
            Err(current) if current == raw => Ok(()),
            Err(_) => Err(AxError::BadState),
        }
    }
}

const fn decode(raw: u64) -> Option<ThreadId> {
    if raw == 0 {
        None
    } else {
        Some(ThreadId::from_parts(raw as u32, (raw >> 32) as u32))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_slot_generation_when_bound() {
        let identity = SchedulerIdentity::unbound();
        let id = ThreadId::from_parts(7, 3);

        identity.bind(id).unwrap();

        assert_eq!(identity.get(), Some(id));
    }

    #[test]
    fn rejects_rebinding_to_a_reused_slot() {
        let identity = SchedulerIdentity::unbound();
        identity.bind(ThreadId::from_parts(7, 3)).unwrap();

        let result = identity.bind(ThreadId::from_parts(7, 4));

        assert_eq!(result, Err(AxError::BadState));
        assert_eq!(identity.get(), Some(ThreadId::from_parts(7, 3)));
    }
}
