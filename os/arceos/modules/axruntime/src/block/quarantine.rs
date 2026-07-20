//! Bounded ownership retention for block endpoints that cannot be destroyed.

use alloc::vec::Vec;

use ax_kspin::SpinNoPreempt;
use rdif_block::{BlkError, QuarantinedQueue, QueueCloseFailure, QueueHandle, QueueInfo};
use thiserror::Error;

const QUEUE_QUARANTINE_CAPACITY: usize = 256;

enum QueueQuarantineSlot {
    Free,
    Reserved(Option<QueueInfo>),
    Occupied(QuarantinedQueue),
}

/// Failure to reserve shutdown-lifetime ownership before accepting a queue.
#[derive(Clone, Copy, Debug, Error, Eq, PartialEq)]
#[error("block queue quarantine capacity is exhausted")]
pub(super) struct QueueQuarantineCapacity;

/// Slots reserved before a controller transfers any queue owner to runtime.
///
/// Unbound slots are safe to release because they have never represented a
/// queue endpoint. Once [`Self::bind`] returns a linear reservation, only an
/// explicit successful close may release that slot.
pub(super) struct QueueQuarantineReservations {
    slots: Vec<usize>,
}

/// Linear shutdown-lifetime capacity for exactly one accepted queue.
///
/// Dropping this token deliberately leaves the registry slot reserved. That
/// fail-closed behavior prevents an unexpected runtime-object drop from making
/// capacity reusable while its portable endpoint is still hardware-visible.
#[must_use = "an accepted queue must release or fill its quarantine reservation"]
pub(super) struct QueueQuarantineReservation {
    slot: usize,
    info: QueueInfo,
}

/// Shutdown-lifetime registry for endpoints that may remain hardware-visible.
///
/// Capacity is reserved before queue materialization. A slot then either gets
/// released by a successful explicit close or permanently owns the complete
/// portable queue object. Consequently no close failure needs allocation and
/// exhaustion is detected before IRQ registration or device publication.
struct QueueQuarantineRegistry {
    slots: [QueueQuarantineSlot; QUEUE_QUARANTINE_CAPACITY],
}

impl QueueQuarantineRegistry {
    const fn new() -> Self {
        Self {
            slots: [const { QueueQuarantineSlot::Free }; QUEUE_QUARANTINE_CAPACITY],
        }
    }

    fn reserve_into(
        &mut self,
        count: usize,
        reserved: &mut Vec<usize>,
    ) -> Result<(), QueueQuarantineCapacity> {
        if self
            .slots
            .iter()
            .filter(|slot| matches!(slot, QueueQuarantineSlot::Free))
            .count()
            < count
        {
            return Err(QueueQuarantineCapacity);
        }

        for (index, slot) in self.slots.iter_mut().enumerate() {
            if reserved.len() == count {
                break;
            }
            if matches!(slot, QueueQuarantineSlot::Free) {
                *slot = QueueQuarantineSlot::Reserved(None);
                reserved.push(index);
            }
        }
        Ok(())
    }

    fn bind(&mut self, slot: usize, info: QueueInfo) {
        let entry = self
            .slots
            .get_mut(slot)
            .expect("queue quarantine reservation index is valid");
        assert!(
            matches!(entry, QueueQuarantineSlot::Reserved(None)),
            "queue quarantine reservation was already bound"
        );
        *entry = QueueQuarantineSlot::Reserved(Some(info));
    }

    fn release_unbound(&mut self, slot: usize) {
        let entry = self
            .slots
            .get_mut(slot)
            .expect("queue quarantine reservation index is valid");
        assert!(
            matches!(entry, QueueQuarantineSlot::Reserved(None)),
            "only an unbound queue reservation may be released by its pool"
        );
        *entry = QueueQuarantineSlot::Free;
    }

    fn release_bound(&mut self, slot: usize, info: QueueInfo) {
        let entry = self
            .slots
            .get_mut(slot)
            .expect("queue quarantine reservation index is valid");
        assert!(
            matches!(entry, QueueQuarantineSlot::Reserved(Some(bound)) if *bound == info),
            "queue quarantine release does not match its accepted endpoint"
        );
        *entry = QueueQuarantineSlot::Free;
    }

    fn retain(&mut self, slot: usize, info: QueueInfo, queue: QuarantinedQueue) {
        let entry = self
            .slots
            .get_mut(slot)
            .expect("queue quarantine reservation index is valid");
        assert!(
            matches!(entry, QueueQuarantineSlot::Reserved(Some(bound)) if *bound == info),
            "queue quarantine owner does not match its reserved endpoint"
        );
        debug_assert_eq!(queue.info(), info);
        *entry = QueueQuarantineSlot::Occupied(queue);
    }

    fn occupied_count(&self) -> usize {
        self.slots
            .iter()
            .filter_map(|slot| match slot {
                QueueQuarantineSlot::Occupied(queue) => Some(queue.info()),
                QueueQuarantineSlot::Free | QueueQuarantineSlot::Reserved(_) => None,
            })
            .count()
    }

    #[cfg(test)]
    fn counts(&self) -> (usize, usize, usize) {
        self.slots
            .iter()
            .fold((0, 0, 0), |(free, reserved, occupied), slot| match slot {
                QueueQuarantineSlot::Free => (free + 1, reserved, occupied),
                QueueQuarantineSlot::Reserved(_) => (free, reserved + 1, occupied),
                QueueQuarantineSlot::Occupied(queue) => {
                    let _ = queue.info();
                    (free, reserved, occupied + 1)
                }
            })
    }
}

static QUEUE_QUARANTINE: SpinNoPreempt<QueueQuarantineRegistry> =
    SpinNoPreempt::new(QueueQuarantineRegistry::new());

impl QueueQuarantineReservations {
    /// Reserves `count` fail-closed slots as one activation transaction.
    pub(super) fn reserve(count: usize) -> Result<Self, QueueQuarantineCapacity> {
        let mut slots = Vec::with_capacity(count);
        QUEUE_QUARANTINE.lock().reserve_into(count, &mut slots)?;
        Ok(Self { slots })
    }

    /// Binds one pre-reserved slot to the queue whose ownership is accepted.
    pub(super) fn bind(&mut self, info: QueueInfo) -> QueueQuarantineReservation {
        let slot = self
            .slots
            .pop()
            .expect("controller produced more queues than its reserved maximum");
        QUEUE_QUARANTINE.lock().bind(slot, info);
        QueueQuarantineReservation { slot, info }
    }
}

impl Drop for QueueQuarantineReservations {
    fn drop(&mut self) {
        if self.slots.is_empty() {
            return;
        }
        let mut registry = QUEUE_QUARANTINE.lock();
        while let Some(slot) = self.slots.pop() {
            registry.release_unbound(slot);
        }
    }
}

impl QueueQuarantineReservation {
    fn release(self) {
        QUEUE_QUARANTINE.lock().release_bound(self.slot, self.info);
    }

    fn retain(self, queue: QuarantinedQueue) {
        let reason = queue.reason();
        let retained = {
            let mut registry = QUEUE_QUARANTINE.lock();
            registry.retain(self.slot, self.info, queue);
            registry.occupied_count()
        };
        error!(
            "quarantined block queue {} ({reason}); {retained} queue endpoint(s) retained",
            self.info.id
        );
    }
}

pub(super) fn close_or_quarantine(
    queue: QueueHandle,
    reservation: QueueQuarantineReservation,
) -> Result<(), BlkError> {
    match queue.close() {
        Ok(()) => {
            reservation.release();
            Ok(())
        }
        Err(failure) => {
            let error = failure.error();
            retain_close_failure(failure, reservation);
            Err(error)
        }
    }
}

pub(super) fn quarantine_live_queue(
    queue: QueueHandle,
    reason: BlkError,
    reservation: QueueQuarantineReservation,
) {
    reservation.retain(queue.into_quarantine(reason));
}

pub(super) fn retain_unpublished_quarantine(
    queue: QuarantinedQueue,
    reservation: QueueQuarantineReservation,
) {
    reservation.retain(queue);
}

fn retain_close_failure(failure: QueueCloseFailure, reservation: QueueQuarantineReservation) {
    reservation.retain(failure.into_quarantine());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_reservation_is_all_or_nothing() {
        let mut registry = QueueQuarantineRegistry::new();
        let mut first = Vec::with_capacity(QUEUE_QUARANTINE_CAPACITY);
        registry
            .reserve_into(QUEUE_QUARANTINE_CAPACITY, &mut first)
            .unwrap();
        let mut rejected = Vec::with_capacity(1);

        assert_eq!(
            registry.reserve_into(1, &mut rejected),
            Err(QueueQuarantineCapacity)
        );
        assert!(rejected.is_empty());
        assert_eq!(registry.counts(), (0, QUEUE_QUARANTINE_CAPACITY, 0));
    }

    #[test]
    fn only_unbound_reservations_are_rollback_releasable() {
        let mut registry = QueueQuarantineRegistry::new();
        let mut slots = Vec::with_capacity(2);
        registry.reserve_into(2, &mut slots).unwrap();
        let unbound = slots.pop().unwrap();

        registry.release_unbound(unbound);

        assert_eq!(registry.counts(), (QUEUE_QUARANTINE_CAPACITY - 1, 1, 0));
    }
}
