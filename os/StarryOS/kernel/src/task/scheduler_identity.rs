//! Binding between a Linux thread identity and its scheduler registry record.

use ax_errno::{AxError, AxResult};
use ax_std::os::arceos::task::ThreadId;
use ax_sync::spin::SpinNoIrq;

/// A scheduler identity that may be published exactly once.
///
/// The contained [`ThreadId`] includes the registry generation. Keeping it as
/// an opaque value prevents Linux TIDs from being used to reconstruct scheduler
/// identities after a registry slot has been reused.
pub(super) struct SchedulerIdentity {
    id: SpinNoIrq<Option<ThreadId>>,
}

impl SchedulerIdentity {
    /// Creates an identity slot for a not-yet-published scheduler thread.
    pub(super) const fn unbound() -> Self {
        Self {
            id: SpinNoIrq::new(None),
        }
    }

    /// Returns the bound generation-bearing identity.
    pub(super) fn get(&self) -> Option<ThreadId> {
        *self.id.lock()
    }

    /// Publishes the identity returned by scheduler thread creation.
    ///
    /// Repeating the same publication is harmless. A different identity means
    /// one Starry thread object was attached to two scheduler records.
    pub(super) fn bind(&self, id: ThreadId) -> AxResult<()> {
        let mut bound = self.id.lock();
        match *bound {
            Some(current) if current != id => Err(AxError::BadState),
            Some(_) => Ok(()),
            None => {
                *bound = Some(id);
                Ok(())
            }
        }
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
