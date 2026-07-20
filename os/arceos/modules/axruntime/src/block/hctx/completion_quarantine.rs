//! DMA-proof-gated retention for completions rejected by request publication.

use alloc::boxed::Box;

use ax_kspin::SpinNoPreempt;
use rdif_block::{BlkError, CompletedRequest, ControllerEpoch, DmaQuiesced};

use super::{HardwareQueueError, MAX_REQUESTS};

const COMPLETION_QUARANTINE_CAPACITY: usize = 256;

/// Rejected completion that retains the complete request owner for quarantine.
pub(super) struct CompletionPublicationError {
    error: HardwareQueueError,
    completion: CompletedRequest,
}

impl CompletionPublicationError {
    pub(super) fn new(error: HardwareQueueError, completion: CompletedRequest) -> Self {
        Self { error, completion }
    }

    pub(super) fn into_parts(self) -> (HardwareQueueError, CompletedRequest) {
        (self.error, self.completion)
    }
}

/// Fixed storage for ownership-bearing completions rejected by the tag table.
///
/// Entries are released only after controller recovery supplies a matching,
/// newer DMA-quiescence proof. A hardware queue can accept at most
/// [`MAX_REQUESTS`] unique request owners. Once all of those retention lanes
/// are occupied, an additional rejected value cannot represent another unique
/// accepted owner; it is returned to the caller for an explicit Rust Drop
/// after the queue has entered fatal recovery.
pub(super) struct RejectedCompletionQuarantine {
    entries: [Option<CompletedRequest>; MAX_REQUESTS],
    len: usize,
    controller_cookie: usize,
    reclaimed_epoch: ControllerEpoch,
}

impl RejectedCompletionQuarantine {
    pub(super) const fn new(controller_cookie: usize) -> Self {
        Self {
            entries: [const { None }; MAX_REQUESTS],
            len: 0,
            controller_cookie,
            reclaimed_epoch: ControllerEpoch::INITIAL,
        }
    }

    pub(super) fn retain(&mut self, rejected: CompletionPublicationError) -> QuarantineRetention {
        let (error, completion) = rejected.into_parts();
        self.retain_completion(error, completion)
    }

    pub(super) fn retain_completion(
        &mut self,
        error: HardwareQueueError,
        completion: CompletedRequest,
    ) -> QuarantineRetention {
        if self.len < self.entries.len() {
            self.entries[self.len] = Some(completion);
            self.len += 1;
            return QuarantineRetention::Retained(error);
        }
        QuarantineRetention::Excess { error, completion }
    }

    pub(super) fn release_after_dma_quiesce(
        &mut self,
        proof: &DmaQuiesced,
    ) -> Result<(), HardwareQueueError> {
        if proof.controller_cookie() != self.controller_cookie
            || proof.epoch() <= self.reclaimed_epoch
        {
            return Err(HardwareQueueError::Driver(BlkError::InvalidDmaProof));
        }
        for entry in &mut self.entries[..self.len] {
            drop(entry.take());
        }
        self.len = 0;
        self.reclaimed_epoch = proof.epoch();
        Ok(())
    }

    pub(super) fn has_retained(&self) -> bool {
        self.len != 0
    }
}

pub(super) enum QuarantineRetention {
    Retained(HardwareQueueError),
    Excess {
        error: HardwareQueueError,
        completion: CompletedRequest,
    },
}

enum CompletionQuarantineSlot {
    Free,
    Reserved {
        queue_id: usize,
        controller_cookie: usize,
    },
    Occupied {
        queue_id: usize,
        quarantine: Box<RejectedCompletionQuarantine>,
    },
}

/// Shutdown-lifetime registry for rejected owners that outlive their hctx.
struct CompletionQuarantineRegistry {
    slots: [CompletionQuarantineSlot; COMPLETION_QUARANTINE_CAPACITY],
}

impl CompletionQuarantineRegistry {
    const fn new() -> Self {
        Self {
            slots: [const { CompletionQuarantineSlot::Free }; COMPLETION_QUARANTINE_CAPACITY],
        }
    }

    fn reserve(
        &mut self,
        queue_id: usize,
        controller_cookie: usize,
    ) -> Option<CompletionQuarantineReservation> {
        let (slot, entry) = self
            .slots
            .iter_mut()
            .enumerate()
            .find(|(_, slot)| matches!(slot, CompletionQuarantineSlot::Free))?;
        *entry = CompletionQuarantineSlot::Reserved {
            queue_id,
            controller_cookie,
        };
        Some(CompletionQuarantineReservation {
            slot,
            queue_id,
            controller_cookie,
        })
    }

    fn release(&mut self, reservation: CompletionQuarantineReservation) {
        let entry = self
            .slots
            .get_mut(reservation.slot)
            .expect("completion quarantine reservation index is valid");
        assert!(
            matches!(
                entry,
                CompletionQuarantineSlot::Reserved {
                    queue_id,
                    controller_cookie,
                } if *queue_id == reservation.queue_id
                    && *controller_cookie == reservation.controller_cookie
            ),
            "completion quarantine release must match its hctx reservation"
        );
        *entry = CompletionQuarantineSlot::Free;
    }

    fn retain(
        &mut self,
        reservation: CompletionQuarantineReservation,
        quarantine: Box<RejectedCompletionQuarantine>,
    ) {
        let entry = self
            .slots
            .get_mut(reservation.slot)
            .expect("completion quarantine reservation index is valid");
        assert!(
            matches!(
                entry,
                CompletionQuarantineSlot::Reserved {
                    queue_id,
                    controller_cookie,
                } if *queue_id == reservation.queue_id
                    && *controller_cookie == reservation.controller_cookie
            ),
            "completion quarantine owner must match its hctx reservation"
        );
        assert_eq!(
            quarantine.controller_cookie, reservation.controller_cookie,
            "completion quarantine controller identity changed"
        );
        *entry = CompletionQuarantineSlot::Occupied {
            queue_id: reservation.queue_id,
            quarantine,
        };
    }

    fn retained_summary(&self) -> (usize, usize) {
        self.slots
            .iter()
            .fold((0, 0), |(queues, owners), slot| match slot {
                CompletionQuarantineSlot::Occupied { quarantine, .. } => {
                    (queues + 1, owners + quarantine.len)
                }
                CompletionQuarantineSlot::Free | CompletionQuarantineSlot::Reserved { .. } => {
                    (queues, owners)
                }
            })
    }

    fn contains_queue(&self, expected_queue_id: usize) -> bool {
        self.slots.iter().any(|slot| {
            matches!(
                slot,
                CompletionQuarantineSlot::Occupied { queue_id, .. }
                    if *queue_id == expected_queue_id
            )
        })
    }

    #[cfg(test)]
    fn counts(&self) -> (usize, usize, usize) {
        self.slots
            .iter()
            .fold((0, 0, 0), |(free, reserved, occupied), slot| match slot {
                CompletionQuarantineSlot::Free => (free + 1, reserved, occupied),
                CompletionQuarantineSlot::Reserved { .. } => (free, reserved + 1, occupied),
                CompletionQuarantineSlot::Occupied {
                    queue_id,
                    quarantine,
                } => {
                    let _ = (*queue_id, quarantine.len);
                    (free, reserved, occupied + 1)
                }
            })
    }
}

static COMPLETION_QUARANTINE: SpinNoPreempt<CompletionQuarantineRegistry> =
    SpinNoPreempt::new(CompletionQuarantineRegistry::new());

/// Pre-reserved named owner for one hctx's rejected completion storage.
///
/// The reservation is acquired before the hctx is published. It is released
/// only when the hctx reaches Drop with no proof-gated owners; otherwise the
/// complete boxed quarantine is transferred into the shutdown-lifetime
/// registry without allocating in Drop.
pub(super) struct CompletionQuarantineReservation {
    slot: usize,
    queue_id: usize,
    controller_cookie: usize,
}

impl CompletionQuarantineReservation {
    pub(super) fn reserve(queue_id: usize, controller_cookie: usize) -> Option<Self> {
        COMPLETION_QUARANTINE
            .lock()
            .reserve(queue_id, controller_cookie)
    }

    pub(super) fn release(self) {
        COMPLETION_QUARANTINE.lock().release(self);
    }

    pub(super) fn retain(self, quarantine: Box<RejectedCompletionQuarantine>) {
        let queue_id = self.queue_id;
        let (retained_queues, retained_owners) = {
            let mut registry = COMPLETION_QUARANTINE.lock();
            registry.retain(self, quarantine);
            assert!(
                registry.contains_queue(queue_id),
                "retained completion owner must remain discoverable by hctx ID"
            );
            registry.retained_summary()
        };
        error!(
            "retained rejected block completion owners for hctx {queue_id}; {retained_owners} \
             owner(s) across {retained_queues} hctx(s)"
        );
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use core::{
        alloc::Layout,
        num::NonZeroUsize,
        ptr::NonNull,
        sync::atomic::{AtomicUsize, Ordering},
    };
    use std::{
        alloc::{alloc_zeroed, dealloc},
        sync::Mutex,
    };

    use dma_api::{
        CpuDmaBuffer, DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError,
        DmaMapHandle, DmaOp,
    };
    use rdif_block::{OwnedRequest, RequestFlags, RequestId, RequestOp};

    use super::*;

    static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static COUNTING_DMA: CountingDma = CountingDma;
    static COUNTING_DMA_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct CountingDma;

    impl DmaOp for CountingDma {
        fn page_size(&self) -> usize {
            4096
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            let pointer = NonNull::new(unsafe { alloc_zeroed(layout) })?;
            Some(unsafe {
                // SAFETY: alloc_zeroed returned this live allocation with the
                // exact layout retained in the handle.
                DmaAllocHandle::new(pointer, (pointer.as_ptr() as u64).into(), layout)
            })
        }

        unsafe fn dealloc_contiguous(&self, handle: DmaAllocHandle) {
            DEALLOCATIONS.fetch_add(1, Ordering::AcqRel);
            unsafe {
                // SAFETY: the handle came from alloc_contiguous above and is
                // consumed exactly once by the DMA allocation owner.
                dealloc(handle.as_ptr().as_ptr(), handle.layout());
            }
        }

        unsafe fn alloc_coherent(
            &self,
            constraints: DmaConstraints,
            layout: Layout,
        ) -> Option<DmaAllocHandle> {
            unsafe { self.alloc_contiguous(constraints, layout) }
        }

        unsafe fn dealloc_coherent(&self, handle: DmaAllocHandle) {
            unsafe { self.dealloc_contiguous(handle) }
        }

        unsafe fn map_streaming(
            &self,
            _constraints: DmaConstraints,
            address: NonNull<u8>,
            size: NonZeroUsize,
            _direction: DmaDirection,
        ) -> Result<DmaMapHandle, DmaError> {
            let layout = Layout::from_size_align(size.get(), 1)?;
            Ok(unsafe {
                // SAFETY: the caller owns address..address+size for this test
                // streaming-map lifetime.
                DmaMapHandle::new(address, (address.as_ptr() as u64).into(), layout, None)
            })
        }

        unsafe fn unmap_streaming(&self, _handle: DmaMapHandle) {}
    }

    #[test]
    fn release_requires_a_new_matching_controller_dma_proof() {
        const CONTROLLER_COOKIE: usize = 0x51a7;

        let _test_lock = COUNTING_DMA_TEST_LOCK.lock().unwrap();
        DEALLOCATIONS.store(0, Ordering::Release);
        let mut quarantine = RejectedCompletionQuarantine::new(CONTROLLER_COOKIE);
        let completion =
            CompletedRequest::new(RequestId::new(64), Ok(()), request_with_counted_dma());
        assert!(matches!(
            quarantine.retain_completion(HardwareQueueError::StaleCompletion, completion),
            QuarantineRetention::Retained(HardwareQueueError::StaleCompletion)
        ));

        let wrong_controller = unsafe {
            // SAFETY: negative-test proof; it never authorizes DMA reuse.
            DmaQuiesced::new(ControllerEpoch::new(2), CONTROLLER_COOKIE + 1)
        };
        assert!(
            quarantine
                .release_after_dma_quiesce(&wrong_controller)
                .is_err()
        );
        assert_eq!(DEALLOCATIONS.load(Ordering::Acquire), 0);

        let replayed_epoch = unsafe {
            // SAFETY: negative-test proof; it never authorizes DMA reuse.
            DmaQuiesced::new(ControllerEpoch::INITIAL, CONTROLLER_COOKIE)
        };
        assert!(
            quarantine
                .release_after_dma_quiesce(&replayed_epoch)
                .is_err()
        );
        assert_eq!(DEALLOCATIONS.load(Ordering::Acquire), 0);

        let matching = unsafe {
            // SAFETY: no real device observes the counting allocation, so the
            // test knows it is quiescent for this controller and epoch.
            DmaQuiesced::new(ControllerEpoch::new(2), CONTROLLER_COOKIE)
        };
        quarantine.release_after_dma_quiesce(&matching).unwrap();
        assert_eq!(DEALLOCATIONS.load(Ordering::Acquire), 1);
    }

    #[test]
    fn excess_rejected_owner_is_returned_for_explicit_drop_without_leaking() {
        const CONTROLLER_COOKIE: usize = 0x51a8;

        let _test_lock = COUNTING_DMA_TEST_LOCK.lock().unwrap();
        DEALLOCATIONS.store(0, Ordering::Release);
        let mut quarantine = RejectedCompletionQuarantine::new(CONTROLLER_COOKIE);
        for request in 0..MAX_REQUESTS {
            let retention = quarantine.retain_completion(
                HardwareQueueError::StaleCompletion,
                CompletedRequest::new(RequestId::new(request), Ok(()), request_with_counted_dma()),
            );
            assert!(matches!(retention, QuarantineRetention::Retained(_)));
        }

        let excess = quarantine.retain_completion(
            HardwareQueueError::StaleCompletion,
            CompletedRequest::new(
                RequestId::new(MAX_REQUESTS),
                Ok(()),
                request_with_counted_dma(),
            ),
        );
        let QuarantineRetention::Excess { completion, .. } = excess else {
            panic!("the accepted-owner bound must return excess ownership");
        };
        assert_eq!(DEALLOCATIONS.load(Ordering::Acquire), 0);
        drop(completion);
        assert_eq!(DEALLOCATIONS.load(Ordering::Acquire), 1);

        let proof = unsafe {
            // SAFETY: no device observes these test allocations.
            DmaQuiesced::new(ControllerEpoch::new(2), CONTROLLER_COOKIE)
        };
        quarantine.release_after_dma_quiesce(&proof).unwrap();
        assert_eq!(DEALLOCATIONS.load(Ordering::Acquire), MAX_REQUESTS + 1);
    }

    #[test]
    fn reserved_registry_slot_retains_the_complete_boxed_quarantine() {
        let mut registry = CompletionQuarantineRegistry::new();
        let reservation = registry.reserve(7, 0x77).unwrap();
        let mut quarantine = Box::new(RejectedCompletionQuarantine::new(0x77));
        quarantine.retain_completion(
            HardwareQueueError::StaleCompletion,
            CompletedRequest::new(RequestId::new(1), Ok(()), flush_request()),
        );

        registry.retain(reservation, quarantine);

        assert_eq!(
            registry.counts(),
            (COMPLETION_QUARANTINE_CAPACITY - 1, 0, 1)
        );
    }

    fn request_with_counted_dma() -> OwnedRequest {
        let device = DeviceDma::new_legacy(u64::MAX, &COUNTING_DMA);
        let data = CpuDmaBuffer::new_zero(
            &device,
            NonZeroUsize::new(1).unwrap(),
            1,
            DmaDirection::FromDevice,
        )
        .unwrap();
        OwnedRequest {
            op: RequestOp::Read,
            lba: 0,
            block_count: 1,
            data: Some(data),
            flags: RequestFlags::NONE,
        }
    }

    fn flush_request() -> OwnedRequest {
        OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        }
    }
}
