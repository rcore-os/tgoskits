// Coalesced wake handshake for one fixed forwarded-IRQ owner.

use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const NO_OWNER: usize = usize::MAX;

pub(crate) struct FixedOwnerContext(AtomicUsize);

impl FixedOwnerContext {
    pub(crate) const fn new() -> Self {
        Self(AtomicUsize::new(NO_OWNER))
    }

    pub(crate) fn install(&self, context_id: usize) -> bool {
        match self
            .0
            .compare_exchange(NO_OWNER, context_id, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(owner) => owner == context_id,
        }
    }

    pub(crate) fn is_owner(&self, context_id: usize) -> bool {
        self.0.load(Ordering::Acquire) == context_id
    }

    pub(crate) fn get(&self) -> Option<usize> {
        let context_id = self.0.load(Ordering::Acquire);
        (context_id != NO_OWNER).then_some(context_id)
    }

    pub(crate) fn clear(&self, context_id: usize) -> bool {
        // Revocation may retry after the owner was already cleared. A
        // different live owner is never treated as completion.
        match self
            .0
            .compare_exchange(context_id, NO_OWNER, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => true,
            Err(owner) => owner == NO_OWNER,
        }
    }
}

pub(crate) struct OwnerDoorbell(AtomicBool);

impl OwnerDoorbell {
    pub(crate) const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    pub(crate) fn clear(&self) {
        self.0.store(false, Ordering::Release);
    }

    pub(crate) fn publish_if(
        &self,
        pending: impl Fn() -> bool,
        mut wake_owner: impl FnMut() -> bool,
    ) -> bool {
        if !pending() || self.0.swap(true, Ordering::AcqRel) {
            return true;
        }
        if wake_owner() {
            return true;
        }
        self.retry_after_failed_wake(pending, wake_owner)
    }

    pub(crate) fn rearm_after_drain(
        &self,
        pending: impl Fn() -> bool,
        mut wake_owner: impl FnMut() -> bool,
    ) -> bool {
        self.0.store(false, Ordering::Release);
        core::sync::atomic::fence(Ordering::Acquire);
        if !pending() || self.0.swap(true, Ordering::AcqRel) {
            return true;
        }
        if wake_owner() {
            return true;
        }
        self.retry_after_failed_wake(pending, wake_owner)
    }

    fn retry_after_failed_wake(
        &self,
        pending: impl Fn() -> bool,
        mut wake_owner: impl FnMut() -> bool,
    ) -> bool {
        self.0.store(false, Ordering::Release);
        core::sync::atomic::fence(Ordering::Acquire);
        if !pending() || self.0.swap(true, Ordering::AcqRel) {
            return true;
        }
        wake_owner()
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use super::*;

    #[test]
    fn repeated_publication_coalesces_to_one_owner_wake() {
        let doorbell = OwnerDoorbell::new();
        let wakes = Cell::new(0);

        assert!(doorbell.publish_if(|| true, || count_wake(&wakes)));
        assert!(doorbell.publish_if(|| true, || count_wake(&wakes)));

        assert_eq!(wakes.get(), 1);
    }

    #[test]
    fn fixed_owner_rejects_nonowner_consumers() {
        let owner = FixedOwnerContext::new();

        assert!(owner.install(1));
        assert!(owner.install(1));
        assert!(!owner.install(3));
        assert!(owner.is_owner(1));
        assert!(!owner.is_owner(3));
        assert_eq!(owner.get(), Some(1));
    }

    #[test]
    fn fixed_owner_clear_is_retryable_but_never_clears_another_owner() {
        let owner = FixedOwnerContext::new();

        assert!(owner.install(1));
        assert!(!owner.clear(3));
        assert_eq!(owner.get(), Some(1));
        assert!(owner.clear(1));
        assert!(owner.clear(1));
        assert_eq!(owner.get(), None);
        assert!(owner.install(3));
        assert_eq!(owner.get(), Some(3));
    }

    #[test]
    fn bounded_drain_rearms_owner_when_completion_work_remains() {
        let doorbell = OwnerDoorbell::new();
        let pending = Cell::new(true);
        let wakes = Cell::new(0);

        assert!(doorbell.publish_if(|| pending.get(), || count_wake(&wakes)));
        assert!(doorbell.rearm_after_drain(|| pending.get(), || count_wake(&wakes)));
        pending.set(false);
        assert!(doorbell.rearm_after_drain(|| pending.get(), || count_wake(&wakes)));
        pending.set(true);
        assert!(doorbell.publish_if(|| pending.get(), || count_wake(&wakes)));

        assert_eq!(wakes.get(), 3);
    }

    #[test]
    fn failed_wake_gets_one_bounded_retry() {
        let doorbell = OwnerDoorbell::new();
        let attempts = Cell::new(0);

        let published = doorbell.publish_if(
            || true,
            || {
                attempts.set(attempts.get() + 1);
                attempts.get() == 2
            },
        );

        assert!(published);
        assert_eq!(attempts.get(), 2);
    }

    #[test]
    fn route_release_clears_the_completion_doorbell_for_reuse() {
        let doorbell = OwnerDoorbell::new();
        let wakes = Cell::new(0);

        assert!(doorbell.publish_if(|| true, || count_wake(&wakes)));
        doorbell.clear();
        assert!(doorbell.publish_if(|| true, || count_wake(&wakes)));

        assert_eq!(wakes.get(), 2);
    }

    fn count_wake(wakes: &Cell<usize>) -> bool {
        wakes.set(wakes.get() + 1);
        true
    }
}
