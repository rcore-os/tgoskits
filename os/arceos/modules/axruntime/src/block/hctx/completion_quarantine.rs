//! DMA-proof-gated retention for completions rejected by request publication.

use core::mem::ManuallyDrop;

use rdif_block::{BlkError, CompletedRequest, ControllerEpoch, DmaQuiesced};

use super::{HardwareQueueError, MAX_REQUESTS};

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
/// Ordinary entries are released only after controller recovery supplies a
/// matching, newer DMA-quiescence proof. The single poison entry is retained
/// for the shutdown lifetime because exceeding the accepted-request capacity
/// means the driver fabricated ownership that cannot be validated.
pub(super) struct RejectedCompletionQuarantine {
    entries: [Option<CompletedRequest>; MAX_REQUESTS],
    len: usize,
    poison: Option<CompletedRequest>,
    controller_cookie: usize,
    reclaimed_epoch: ControllerEpoch,
}

impl RejectedCompletionQuarantine {
    pub(super) const fn new(controller_cookie: usize) -> Self {
        Self {
            entries: [const { None }; MAX_REQUESTS],
            len: 0,
            poison: None,
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
        if self.poison.is_none() {
            self.poison = Some(completion);
            return QuarantineRetention::Poisoned(error);
        }

        // One extra malformed owner already consumes the only representable
        // poison lane. Preserve this second value through the fatal invariant
        // rather than invoking Drop on potentially live or duplicate DMA.
        let _unrepresentable_owner = ManuallyDrop::new(completion);
        panic!("block hctx produced more than one unrepresentable completion owner");
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
}

pub(super) enum QuarantineRetention {
    Retained(HardwareQueueError),
    Poisoned(HardwareQueueError),
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
    use std::alloc::{alloc_zeroed, dealloc};

    use dma_api::{
        CpuDmaBuffer, DeviceDma, DmaAllocHandle, DmaConstraints, DmaDirection, DmaError,
        DmaMapHandle, DmaOp,
    };
    use rdif_block::{OwnedRequest, RequestFlags, RequestId, RequestOp};

    use super::*;

    static DEALLOCATIONS: AtomicUsize = AtomicUsize::new(0);
    static COUNTING_DMA: CountingDma = CountingDma;

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
}
