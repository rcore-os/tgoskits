//! Nested-table retirement while guests are outside hardware execution.
//!
//! A table update closes admission before requesting exits. It takes no VM
//! machine lock until all admitted guests have returned, avoiding an IRQ-off
//! machine-lock / synchronous remote-call cycle. The next hardware entry flushes
//! that CPU's nested translation context before any retired mapping can be used again.

use std::sync::atomic::{AtomicUsize, Ordering};

const UPDATING: usize = 1usize << (usize::BITS - 1);

pub(super) struct TranslationGate(AtomicUsize);

impl TranslationGate {
    pub(super) const fn new() -> Self {
        Self(AtomicUsize::new(0))
    }

    pub(crate) fn enter(&self) -> Option<GuestTranslation<'_>> {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & UPDATING == 0 && state < UPDATING - 1).then_some(state + 1)
            })
            .ok()
            .map(|_| GuestTranslation(self))
    }

    pub(super) fn begin_update(&self) -> Option<TranslationUpdate<'_>> {
        self.0
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & UPDATING == 0).then_some(state | UPDATING)
            })
            .ok()
            .map(|_| TranslationUpdate(self))
    }
}

pub(crate) struct GuestTranslation<'a>(&'a TranslationGate);
impl Drop for GuestTranslation<'_> {
    fn drop(&mut self) {
        self.0.0.fetch_sub(1, Ordering::Release);
    }
}

pub(super) struct TranslationUpdate<'a>(&'a TranslationGate);
impl TranslationUpdate<'_> {
    pub(super) fn quiescent(&self) -> bool {
        self.0.0.load(Ordering::Acquire) == UPDATING
    }
}
impl Drop for TranslationUpdate<'_> {
    fn drop(&mut self) {
        self.0.0.fetch_and(!UPDATING, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::TranslationGate;

    #[test]
    fn update_closes_admission_until_every_guest_retires() {
        let gate = TranslationGate::new();
        let first = gate.enter().unwrap();
        let second = gate.enter().unwrap();
        let update = gate.begin_update().unwrap();
        assert!(gate.enter().is_none());
        assert!(gate.begin_update().is_none());
        assert!(!update.quiescent());
        drop(first);
        assert!(!update.quiescent());
        drop(second);
        assert!(update.quiescent());
        drop(update);
        assert!(gate.enter().is_some());
    }

    #[test]
    fn canceled_update_reopens_admission_without_losing_live_guests() {
        let gate = TranslationGate::new();
        let guest = gate.enter().unwrap();
        drop(gate.begin_update().unwrap());
        let other = gate.enter().unwrap();
        let update = gate.begin_update().unwrap();
        drop(guest);
        assert!(!update.quiescent());
        drop(other);
        assert!(update.quiescent());
    }
}
