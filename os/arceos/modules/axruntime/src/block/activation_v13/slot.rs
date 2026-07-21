//! Fixed single-owner transport for move-only IRQ evidence.

use core::{
    cell::UnsafeCell,
    mem::MaybeUninit,
    sync::atomic::{AtomicU8, Ordering},
};

const SLOT_EMPTY: u8 = 0;
const SLOT_WRITING: u8 = 1;
const SLOT_READY: u8 = 2;
const SLOT_TAKING: u8 = 3;
#[cfg(test)]
const SLOT_RETIRED: u8 = 4;

/// Preallocated hard-IRQ producer to maintenance-owner transport.
///
/// A source latch guarantees at most one move-only evidence owner, while this
/// slot bridges that owner into a [`crate::maintenance::MaintenanceMailbox`]
/// whose payload intentionally remains `Copy`. The mailbox carries only the
/// source identity; its owner takes the evidence from this slot exactly once.
pub(super) struct LinearEvidenceSlot<T> {
    state: AtomicU8,
    value: UnsafeCell<MaybeUninit<T>>,
}

impl<T> LinearEvidenceSlot<T> {
    pub(super) const fn new() -> Self {
        Self {
            state: AtomicU8::new(SLOT_EMPTY),
            value: UnsafeCell::new(MaybeUninit::uninit()),
        }
    }

    /// Publishes one move-only owner without allocation or a retry loop.
    #[cfg(test)]
    pub(super) fn publish_from_irq(&self, value: T) -> Result<(), LinearSlotFull<T>> {
        let reservation = match self.try_reserve_from_irq() {
            Ok(reservation) => reservation,
            Err(_) => return Err(LinearSlotFull(value)),
        };
        reservation.commit(value);
        Ok(())
    }

    /// Reserves storage before a device acknowledgement can mint a move-only
    /// protocol owner.
    pub(super) fn try_reserve_from_irq(
        &self,
    ) -> Result<LinearEvidenceReservation<'_, T>, LinearSlotBusy> {
        self.state
            .compare_exchange(
                SLOT_EMPTY,
                SLOT_WRITING,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .map_err(|_| LinearSlotBusy)?;
        Ok(LinearEvidenceReservation {
            slot: self,
            committed: false,
        })
    }

    /// Takes the unique owner after observing its Release publication.
    pub(super) fn take_owner(&self) -> Option<T> {
        if self
            .state
            .compare_exchange(
                SLOT_READY,
                SLOT_TAKING,
                Ordering::Acquire,
                Ordering::Relaxed,
            )
            .is_err()
        {
            return None;
        }
        // SAFETY: READY was published only after initialization, and the
        // successful transition gives the sole maintenance owner exclusive
        // read access before it returns the slot to EMPTY.
        let value = unsafe { (*self.value.get()).assume_init_read() };
        self.state.store(SLOT_EMPTY, Ordering::Release);
        Some(value)
    }

    pub(super) fn has_ready_owner(&self) -> bool {
        self.state.load(Ordering::Acquire) == SLOT_READY
    }

    /// Retires the slot after its producer and consumer have been stopped.
    ///
    /// A ready value is returned to the lifecycle owner instead of being
    /// destroyed by `Drop`. This is required for protocol owners such as an
    /// IRQ mask permit: explicit close may reclaim it, while failed close must
    /// retain it in the named device quarantine.
    #[cfg(test)]
    pub(super) fn retire(&mut self) -> Result<RetiredLinearEvidence<T>, LinearSlotRetireError> {
        match *self.state.get_mut() {
            SLOT_EMPTY => {
                *self.state.get_mut() = SLOT_RETIRED;
                Ok(RetiredLinearEvidence { owner: None })
            }
            SLOT_READY => {
                // SAFETY: `&mut self` excludes the IRQ producer and owner
                // consumer. READY proves that the value is initialized and
                // has not yet been moved out.
                let owner = unsafe { self.value.get_mut().assume_init_read() };
                *self.state.get_mut() = SLOT_RETIRED;
                Ok(RetiredLinearEvidence { owner: Some(owner) })
            }
            SLOT_WRITING => Err(LinearSlotRetireError::ProducerActive),
            SLOT_TAKING => Err(LinearSlotRetireError::ConsumerActive),
            SLOT_RETIRED => Err(LinearSlotRetireError::AlreadyRetired),
            _ => Err(LinearSlotRetireError::InvalidState),
        }
    }
}

/// A preallocated slot reserved before a destructive device capture.
#[must_use = "commit a captured owner or let the reservation abort to EMPTY"]
pub(super) struct LinearEvidenceReservation<'slot, T> {
    slot: &'slot LinearEvidenceSlot<T>,
    committed: bool,
}

impl<T> LinearEvidenceReservation<'_, T> {
    /// Publishes the unique value and makes it visible to the owner.
    pub(super) fn commit(mut self, value: T) {
        // SAFETY: EMPTY -> WRITING gave this reservation the only write
        // access. The owner cannot observe the value before the Release store.
        unsafe { (*self.slot.value.get()).write(value) };
        self.slot.state.store(SLOT_READY, Ordering::Release);
        self.committed = true;
    }
}

impl<T> Drop for LinearEvidenceReservation<'_, T> {
    fn drop(&mut self) {
        if !self.committed {
            // No value was initialized, so aborting the pre-capture
            // reservation only restores admission for a later IRQ.
            self.slot.state.store(SLOT_EMPTY, Ordering::Release);
        }
    }
}

// SAFETY: the atomic state gives the IRQ producer and sole maintenance
// consumer disjoint access to the UnsafeCell. T crosses those contexts by
// ownership transfer and must therefore be Send.
unsafe impl<T: Send> Sync for LinearEvidenceSlot<T> {}

/// Result of explicitly retiring a linear slot.
///
/// The contained owner is deliberately not hidden behind slot destruction.
#[cfg(test)]
pub(super) struct RetiredLinearEvidence<T> {
    owner: Option<T>,
}

#[cfg(test)]
impl<T> RetiredLinearEvidence<T> {
    pub(super) fn into_owner(self) -> Option<T> {
        self.owner
    }
}

/// A slot cannot be retired while an IRQ publication is in progress.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[cfg(test)]
pub(super) enum LinearSlotRetireError {
    ProducerActive,
    ConsumerActive,
    AlreadyRetired,
    InvalidState,
}

/// Failed publication retaining the complete move-only evidence owner.
#[derive(Debug)]
#[cfg(test)]
pub(super) struct LinearSlotFull<T>(pub(super) T);

/// The fixed move-only owner slot already contains or transfers evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct LinearSlotBusy;

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    #[test]
    fn move_only_owner_is_published_and_taken_once() {
        let slot = LinearEvidenceSlot::new();
        let owner = Box::new(41_u64);

        slot.publish_from_irq(owner).unwrap();

        assert!(slot.has_ready_owner());
        assert_eq!(*slot.take_owner().unwrap(), 41);
        assert!(slot.take_owner().is_none());
    }

    #[test]
    fn full_slot_returns_second_owner_without_dropping_it() {
        let drops = Arc::new(AtomicUsize::new(0));
        let slot = LinearEvidenceSlot::new();
        slot.publish_from_irq(DropOwner(Arc::clone(&drops)))
            .unwrap();

        let second = slot
            .publish_from_irq(DropOwner(Arc::clone(&drops)))
            .unwrap_err()
            .0;

        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(second);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        drop(slot.take_owner());
        assert_eq!(drops.load(Ordering::Relaxed), 2);
    }

    #[test]
    fn abandoned_reservation_restores_empty_before_value_exists() {
        let slot = LinearEvidenceSlot::new();
        let reservation = slot.try_reserve_from_irq().unwrap();
        assert!(matches!(slot.try_reserve_from_irq(), Err(LinearSlotBusy)));

        drop(reservation);
        slot.try_reserve_from_irq().unwrap().commit(17_u8);

        assert_eq!(slot.take_owner(), Some(17));
    }

    #[test]
    fn retirement_returns_a_live_owner_instead_of_dropping_it() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut slot = LinearEvidenceSlot::new();
        slot.publish_from_irq(DropOwner(Arc::clone(&drops)))
            .unwrap();

        let retired = slot.retire().unwrap();

        assert_eq!(drops.load(Ordering::Relaxed), 0);
        drop(retired.into_owner());
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[derive(Debug)]
    struct DropOwner(Arc<AtomicUsize>);

    impl Drop for DropOwner {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
}
