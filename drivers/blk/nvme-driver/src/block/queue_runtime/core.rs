//! Queue hardware ownership, completion cache, and exclusion gates.

use alloc::{sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

use dma_api::CoherentArray;
use rdif_block::{
    BlkError, CompletionSink, IdList, InitError, QueueExecution, QueueInfo, QueueKind,
};

use super::{
    super::{
        CompletionCache, CompletionDrain, device_info, drain_owner_completions_to_cache, limits,
    },
    request::NvmeQueueState,
};
use crate::{Namespace, queue::NvmeQueue as HardwareQueue};

pub(in crate::block) const NVME_QUEUE_EXECUTION: QueueExecution = QueueExecution::Tagged;

pub(in crate::block) struct NvmeQueueCore {
    id: usize,
    name: &'static str,
    namespace: Namespace,
    dma_mask: u64,
    page_size: usize,
    max_transfer_bytes: Option<usize>,
    depth: usize,
    interrupt_sources: IdList,
    pub(in crate::block) reinitialize_info: NvmeQueueReinitializeInfo,
    queue: UnsafeCell<HardwareQueue>,
    state: UnsafeCell<NvmeQueueState>,
    completion_cache: UnsafeCell<CompletionCache>,
    state_claimed: AtomicBool,
    completion_fault: AtomicBool,
}

#[derive(Clone, Copy)]
pub(in crate::block) struct NvmeQueueReinitializeInfo {
    pub(in crate::block) qid: u32,
    pub(in crate::block) sq_len: usize,
    pub(in crate::block) cq_len: usize,
    pub(in crate::block) sq_bus_addr: u64,
    pub(in crate::block) cq_bus_addr: u64,
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
        queue: HardwareQueue,
        prp_lists: Vec<CoherentArray<u64>>,
    ) -> Arc<Self> {
        let reinitialize_info = NvmeQueueReinitializeInfo {
            qid: queue.qid,
            sq_len: queue.sq_len(),
            cq_len: queue.cq_len(),
            sq_bus_addr: queue.sq_bus_addr(),
            cq_bus_addr: queue.cq_bus_addr(),
        };

        Arc::new(Self {
            id,
            name,
            namespace,
            dma_mask,
            page_size,
            max_transfer_bytes,
            depth,
            interrupt_sources,
            reinitialize_info,
            queue: UnsafeCell::new(queue),
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
        QueueInfo {
            id: self.id,
            device: device_info(self.name, self.namespace),
            limits: limits(
                self.dma_mask,
                self.page_size,
                self.max_transfer_bytes,
                self.namespace,
                self.depth,
            ),
            kind: QueueKind::Interrupt {
                sources: self.interrupt_sources,
            },
            execution: NVME_QUEUE_EXECUTION,
        }
    }

    pub(super) const fn namespace(&self) -> Namespace {
        self.namespace
    }

    pub(super) const fn page_size(&self) -> usize {
        self.page_size
    }

    pub(super) fn try_claim_state(&self) -> Option<NvmeQueueStateGuard<'_>> {
        AtomicClaimGuard::try_acquire(&self.state_claimed).map(|claim| NvmeQueueStateGuard {
            core: self,
            _claim: claim,
        })
    }

    pub(super) fn completion_failed(&self) -> bool {
        self.completion_fault.load(Ordering::Acquire)
    }

    pub(super) fn submit_command(&self, command: crate::queue::CommandSet) {
        // SAFETY: RDIF gives one CPU-pinned maintenance owner to this queue,
        // so SQ and CQ mutation are serialized in the same domain.
        let queue = unsafe { &*self.queue.get() };
        queue.submit_io_data(command);
    }

    pub(super) fn drain_owner_completions(&self, budget: usize) -> CompletionDrain {
        // SAFETY: `IQueue::service_events` is called only by the queue's
        // CPU-pinned maintenance owner. The acknowledged source remains
        // device-masked, and lifecycle reset cannot run until that owner has
        // closed queue access and drained the IRQ action.
        let queue = unsafe { &*self.queue.get() };
        // SAFETY: the maintenance owner is the only live queue accessor. A
        // lifecycle reset can touch this cache only after that owner and its
        // IRQ action have been drained.
        let cache = unsafe { &mut *self.completion_cache.get() };
        let drain = drain_owner_completions_to_cache(queue, cache, budget);
        self.publish_completion_drain(drain);
        drain
    }

    pub(super) fn emit_owner_cached_completions(
        &self,
        budget: usize,
        sink: &mut dyn CompletionSink,
    ) -> Result<Option<usize>, BlkError> {
        let Some(mut state) = self.try_claim_state() else {
            return Ok(None);
        };
        // SAFETY: only the CPU-pinned maintenance owner emits retained
        // completions. The state claim excludes proof-gated lifecycle reset
        // while request ownership is transferred to the sink.
        let cache = unsafe { &mut *self.completion_cache.get() };
        state
            .emit_cached_completions(self.id, cache, budget, sink)
            .map(Some)
    }

    pub(super) fn service_pending(&self) -> bool {
        // SAFETY: this query runs in the same maintenance-owner scope as
        // completion drain and publication. Lifecycle reset is allowed only
        // after that scope and its IRQ action have been drained.
        let cache = unsafe { &*self.completion_cache.get() };
        cache.has_ready() || self.completion_fault.load(Ordering::Acquire)
    }

    pub(super) fn clear_service_state_after_quiesce(&self) {
        // SAFETY: the caller presents the controller's DMA-quiesced proof and
        // has already reclaimed every accepted request from the owner scope.
        let cache = unsafe { &mut *self.completion_cache.get() };
        cache.clear_after_quiesce();
        self.completion_fault.store(false, Ordering::Release);
    }

    fn publish_completion_drain(&self, drain: CompletionDrain) {
        if drain.invalid {
            self.completion_fault.store(true, Ordering::Release);
        }
    }

    /// Resets retained queue state after the controller stopped DMA.
    ///
    /// # Safety
    ///
    /// The caller must hold the controller's DmaQuiesced proof and keep hctx
    /// driver access plus every IRQ action drained for the duration.
    pub(in crate::block) unsafe fn reset_after_quiesce(&self) -> Result<(), InitError> {
        if self
            .state_claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(InitError::Hardware(
                "NVMe request state remained claimed after hctx drain",
            ));
        }
        // SAFETY: the successful claim grants exclusive request ownership
        // while the runtime keeps hctx access closed.
        let state = unsafe { &mut *self.state.get() };
        if state.has_accepted() {
            self.state_claimed.store(false, Ordering::Release);
            return Err(InitError::Hardware(
                "NVMe request ownership was not reclaimed before reset",
            ));
        }
        // SAFETY: controller RDY is zero, the request-state claim is held, and
        // the maintenance owner has already drained its IRQ action and queue
        // access, so retained queue memory has no concurrent accessor.
        let queue = unsafe { &*self.queue.get() };
        // SAFETY: the method contract and owner claim exclude every device,
        // task, and IRQ access to retained queue memory.
        unsafe { queue.reset_after_controller_disable() };
        // SAFETY: the method contract excludes device, task, and IRQ access.
        let cache = unsafe { &mut *self.completion_cache.get() };
        cache.clear_after_quiesce();
        self.completion_fault.store(false, Ordering::Release);
        self.state_claimed.store(false, Ordering::Release);
        Ok(())
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
