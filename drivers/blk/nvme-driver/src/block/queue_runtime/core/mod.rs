//! Queue hardware ownership, completion cache, and exclusion gates.

use alloc::{sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

use dma_api::CoherentArray;
use rdif_block::{
    AcceptedRequest as AcceptedInterruptRequest, BlkError, CompletionSink, IdList, InitError,
    OwnedRequest, QueueExecution, QueueInfo, QueueKind, RequestId, SubmitError, UnacceptedRequest,
    validate_owned_request,
};

use super::{
    super::{
        CompletionCache, CompletionDrain, device_info, drain_owner_completions_to_cache, limits,
    },
    dma::{prepare_request_dma, restore_prepared_dma},
    request::{AcceptedRequest, NvmeQueueState},
};
use crate::{
    Namespace,
    queue::{NvmeCompletionProbe, NvmeQueue as HardwareQueue},
};

mod completion_owner;
mod submission;

pub(in crate::block) const NVME_QUEUE_EXECUTION: QueueExecution = QueueExecution::Tagged;

pub(in crate::block) struct NvmeQueueCore {
    /// Zero-based queue identity inside the RDIF ownership domain.
    ///
    /// This is deliberately not the NVMe SQID. Hardware reserves QID zero
    /// for admin, so domain slot 63 maps to hardware QID 64.
    id: usize,
    name: &'static str,
    legacy_namespace: Option<Namespace>,
    dma_mask: u64,
    page_size: usize,
    max_transfer_bytes: Option<usize>,
    depth: usize,
    interrupt_sources: IdList,
    pub(in crate::block) reinitialize_info: NvmeQueueReinitializeInfo,
    completion_probe: NvmeCompletionProbe,
    queue: Arc<HardwareQueue>,
    state: UnsafeCell<NvmeQueueState>,
    completion_cache: UnsafeCell<CompletionCache>,
    state_claimed: AtomicBool,
    completion_fault: AtomicBool,
}

pub(in crate::block) struct NvmeQueueEvidenceProgress {
    pub(in crate::block) retained: bool,
}

#[derive(Clone, Copy)]
pub(in crate::block) struct NvmeQueueReinitializeInfo {
    pub(in crate::block) qid: u32,
    pub(in crate::block) sq_len: usize,
    pub(in crate::block) cq_len: usize,
    pub(in crate::block) sq_bus_addr: u64,
    pub(in crate::block) cq_bus_addr: u64,
}

impl NvmeQueueReinitializeInfo {
    pub(in crate::block) fn from_queue(queue: &HardwareQueue) -> Self {
        Self {
            qid: queue.qid,
            sq_len: queue.sq_len(),
            cq_len: queue.cq_len(),
            sq_bus_addr: queue.sq_bus_addr(),
            cq_bus_addr: queue.cq_bus_addr(),
        }
    }
}

pub(super) struct NvmeQueueStateGuard<'a> {
    core: &'a NvmeQueueCore,
    _claim: AtomicClaimGuard<'a>,
}

struct AtomicClaimGuard<'a> {
    claimed: &'a AtomicBool,
}

impl NvmeQueueCore {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::block) fn new(
        id: usize,
        depth: usize,
        name: &'static str,
        namespace: Namespace,
        dma_mask: u64,
        page_size: usize,
        max_transfer_bytes: Option<usize>,
        interrupt_sources: IdList,
        queue: Arc<HardwareQueue>,
        prp_lists: Vec<CoherentArray<u64>>,
    ) -> Arc<Self> {
        Self::new_inner(
            id,
            depth,
            name,
            Some(namespace),
            dma_mask,
            page_size,
            max_transfer_bytes,
            interrupt_sources,
            queue,
            prp_lists,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new_inner(
        id: usize,
        depth: usize,
        name: &'static str,
        legacy_namespace: Option<Namespace>,
        dma_mask: u64,
        page_size: usize,
        max_transfer_bytes: Option<usize>,
        interrupt_sources: IdList,
        queue: Arc<HardwareQueue>,
        prp_lists: Vec<CoherentArray<u64>>,
    ) -> Arc<Self> {
        let reinitialize_info = NvmeQueueReinitializeInfo::from_queue(&queue);
        let completion_probe = queue.completion_probe();

        Arc::new(Self {
            id,
            name,
            legacy_namespace,
            dma_mask,
            page_size,
            max_transfer_bytes,
            depth,
            interrupt_sources,
            reinitialize_info,
            completion_probe,
            queue,
            state: UnsafeCell::new(NvmeQueueState::new(depth, prp_lists)),
            completion_cache: UnsafeCell::new(CompletionCache::new(depth + 1)),
            state_claimed: AtomicBool::new(false),
            completion_fault: AtomicBool::new(false),
        })
    }

    pub(in crate::block) const fn id(&self) -> usize {
        self.id
    }

    pub(super) fn queue_info(&self) -> QueueInfo {
        self.queue_info_for(self.namespace(), self.max_transfer_bytes)
    }

    pub(in crate::block) fn queue_info_for(
        &self,
        namespace: Namespace,
        max_transfer_bytes: Option<usize>,
    ) -> QueueInfo {
        QueueInfo {
            id: self.id,
            device: device_info(self.name, namespace),
            limits: limits(
                self.dma_mask,
                self.page_size,
                max_transfer_bytes,
                namespace,
                self.depth,
            ),
            kind: QueueKind::Interrupt {
                sources: self.interrupt_sources,
            },
            execution: NVME_QUEUE_EXECUTION,
        }
    }

    pub(in crate::block) fn completion_probe(&self) -> NvmeCompletionProbe {
        self.completion_probe.clone()
    }

    pub(super) const fn namespace(&self) -> Namespace {
        match self.legacy_namespace {
            Some(namespace) => namespace,
            None => panic!("a prepared NVMe queue has no legacy namespace binding"),
        }
    }

    pub(in crate::block) const fn max_transfer_bytes(&self) -> Option<usize> {
        self.max_transfer_bytes
    }

    pub(super) fn try_claim_state(&self) -> Option<NvmeQueueStateGuard<'_>> {
        AtomicClaimGuard::try_acquire(&self.state_claimed).map(|claim| NvmeQueueStateGuard {
            core: self,
            _claim: claim,
        })
    }
}

// SAFETY: Slot, CID, SQ, CQ, and completion-cache access is driven only by the
// single CPU-pinned maintenance owner. `state_claimed` additionally excludes
// proof-gated lifecycle reset. MMIO and DMA storage outlive the core through
// the owner/interface lifetime.
unsafe impl Send for NvmeQueueCore {}

// SAFETY: hard IRQ has no reference to this type. Shared references exist only
// for the maintenance owner and a lifecycle reset that requires IRQ drain and
// matching DMA-quiescence proof before touching retained queue storage.
unsafe impl Sync for NvmeQueueCore {}

impl Deref for NvmeQueueStateGuard<'_> {
    type Target = NvmeQueueState;

    fn deref(&self) -> &Self::Target {
        // SAFETY: construction requires the successful `state_claimed`
        // transition, excluding every other state guard until Drop.
        unsafe { &*self.core.state.get() }
    }
}

impl DerefMut for NvmeQueueStateGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        // SAFETY: this guard uniquely owns the `state_claimed` token.
        unsafe { &mut *self.core.state.get() }
    }
}

impl AtomicClaimGuard<'_> {
    fn try_acquire(claimed: &AtomicBool) -> Option<AtomicClaimGuard<'_>> {
        if claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        Some(AtomicClaimGuard { claimed })
    }
}

impl Drop for AtomicClaimGuard<'_> {
    fn drop(&mut self) {
        self.claimed.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use core::sync::atomic::{AtomicBool, Ordering};

    use super::AtomicClaimGuard;

    #[test]
    fn failed_contender_does_not_release_the_owner_claim() {
        let claimed = AtomicBool::new(false);
        let owner = AtomicClaimGuard::try_acquire(&claimed).expect("first claimant must own token");

        assert!(AtomicClaimGuard::try_acquire(&claimed).is_none());
        assert!(
            claimed.load(Ordering::Acquire),
            "failed contender must not release another owner's token"
        );

        drop(owner);
        assert!(!claimed.load(Ordering::Acquire));
    }
}
