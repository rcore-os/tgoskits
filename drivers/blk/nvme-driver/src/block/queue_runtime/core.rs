//! Queue hardware ownership, completion cache, and exclusion gates.

use alloc::{sync::Arc, vec::Vec};
use core::{
    cell::UnsafeCell,
    ops::{Deref, DerefMut},
    sync::atomic::{AtomicBool, Ordering},
};

use dma_api::CoherentArray;
use rdif_block::{DispatchMode, IdList, InitError, QueueInfo, QueueKind};

use super::{
    super::{
        CompletionCache, CompletionDrain, IrqCompletionContinuation, device_info,
        drain_hardware_completions_to_cache, limits,
    },
    request::NvmeQueueState,
};
use crate::{Namespace, queue::NvmeQueue as HardwareQueue};

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
    completion_cache: CompletionCache,
    state_claimed: AtomicBool,
    cq_claimed: AtomicBool,
    cq_continuation: IrqCompletionContinuation,
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
            completion_cache: CompletionCache::new(depth + 1),
            state_claimed: AtomicBool::new(false),
            cq_claimed: AtomicBool::new(false),
            cq_continuation: IrqCompletionContinuation::new(),
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
            dispatch_mode: DispatchMode::Direct,
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

    pub(super) const fn completion_cache(&self) -> &CompletionCache {
        &self.completion_cache
    }

    pub(super) fn completion_failed(&self) -> bool {
        self.completion_fault.load(Ordering::Acquire)
    }

    pub(super) fn submit_command(&self, command: crate::queue::CommandSet) {
        // SAFETY: RDIF gives one task owner to this queue, so SQ mutation is
        // serialized. IRQ/task CQ consumers touch a disjoint queue cell.
        let queue = unsafe { &*self.queue.get() };
        queue.submit_io_data(command);
    }

    pub(in crate::block) fn drain_irq_completions(&self, budget: usize) -> CompletionDrain {
        let Some(drain) = self.try_with_cq_claim(|queue| {
            drain_hardware_completions_to_cache(queue, &self.completion_cache, budget)
        }) else {
            self.cq_continuation.request();
            return CompletionDrain::deferred();
        };
        self.publish_completion_drain(drain);
        drain
    }

    pub(in crate::block) fn request_irq_completion_continuation(&self) {
        self.cq_continuation.request();
    }

    pub(super) fn drain_service_completions(&self, budget: usize) -> Option<CompletionDrain> {
        if !self.cq_continuation.take_for_service() {
            return None;
        }
        let Some(drain) = self.try_with_cq_claim(|queue| {
            drain_hardware_completions_to_cache(queue, &self.completion_cache, budget)
        }) else {
            self.cq_continuation.request();
            return Some(CompletionDrain::deferred());
        };
        self.publish_completion_drain(drain);
        Some(drain)
    }

    pub(super) fn service_pending(&self) -> bool {
        self.cq_continuation.is_pending()
            || self.completion_cache.has_ready()
            || self.completion_fault.load(Ordering::Acquire)
    }

    pub(super) fn clear_service_pending(&self) {
        self.cq_continuation.clear_after_quiesce();
    }

    fn try_with_cq_claim<R>(&self, f: impl FnOnce(&HardwareQueue) -> R) -> Option<R> {
        if self
            .cq_claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return None;
        }
        // SAFETY: `cq_claimed` serializes every IRQ/task consumer of the CQ.
        // SQ submission uses a disjoint `UnsafeCell` inside `HardwareQueue`.
        let queue = unsafe { &*self.queue.get() };
        let result = f(queue);
        self.cq_claimed.store(false, Ordering::Release);
        Some(result)
    }

    fn publish_completion_drain(&self, drain: CompletionDrain) {
        if drain.continuation {
            self.cq_continuation.request();
        }
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
        if self
            .cq_claimed
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            self.state_claimed.store(false, Ordering::Release);
            return Err(InitError::Hardware(
                "NVMe completion queue remained claimed after IRQ drain",
            ));
        }

        // SAFETY: controller RDY is zero and both task and CQ claims are held,
        // so retained queue memory has no concurrent accessor.
        let queue = unsafe { &*self.queue.get() };
        // SAFETY: the method contract and both local claims exclude every
        // device, task, and IRQ access to retained queue memory.
        unsafe { queue.reset_after_controller_disable() };
        self.completion_cache.clear_after_quiesce();
        self.cq_continuation.clear_after_quiesce();
        self.completion_fault.store(false, Ordering::Release);
        self.cq_claimed.store(false, Ordering::Release);
        self.state_claimed.store(false, Ordering::Release);
        Ok(())
    }
}

// SAFETY: Slot, CID, and completion-cache access is serialized through
// `state_claimed`; hardware CQ access is serialized through `cq_claimed`. SQ
// submission is only driven by the single RDIF queue owner. MMIO and DMA
// storage outlive the core through the owner/interface lifetime.
unsafe impl Send for NvmeQueueCore {}

// SAFETY: shared references cross task and hard-IRQ contexts only through the
// exclusion gates documented above.
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
