//! Move-only queue state for the rdif-block v0.13 ownership domain.

use alloc::vec::Vec;
use core::{cell::Cell, marker::PhantomData};

use dma_api::CoherentArray;
use rdif_block::{
    AcceptedRequest as AcceptedInterruptRequest, BlkError, CompletionSink, IdList, OwnedRequest,
    QueueExecution, QueueInfo, QueueKind, RequestId, SubmitError, UnacceptedRequest,
    validate_owned_request,
};

use super::{
    super::{
        CompletionCache, NvmeQueueReinitializeInfo, device_info, drain_owner_completions_to_cache,
        limits,
    },
    dma::{prepare_request_dma, restore_prepared_dma},
    dma_owner::QueueDmaOwner,
    request::{AcceptedRequest, CidAllocationError, NvmeQueueState},
};
use crate::{Namespace, queue::NvmeQueue as HardwareQueue};

/// Queue resources allocated after the runtime selected an exact queue depth.
///
/// Controller initialization retains only immutable SQ/CQ creation geometry;
/// this value is the unique cursor, descriptor-storage, and DMA owner from
/// allocation through final publication.
pub(in crate::block) struct PreparedNvmeOwnedQueue {
    slot: usize,
    hardware_qid: u32,
    reinitialize_info: NvmeQueueReinitializeInfo,
    dma: QueueDmaOwner<NvmeOwnedQueueDma>,
}

/// Queue state owned exclusively by one CPU-pinned maintenance domain.
///
/// Normal submit, CQ service, reclaim, and reset all require `&mut self`.
/// Hard IRQ receives only the independent completion probe created before this
/// owner is published. `Cell` deliberately makes this owner `!Sync` while
/// preserving `Send` for the one move into its maintenance thread.
pub(in crate::block) struct NvmeOwnedQueue {
    slot: usize,
    hardware_qid: u32,
    dma: QueueDmaOwner<NvmeOwnedQueueDma>,
    _not_sync: PhantomData<Cell<()>>,
}

struct NvmeOwnedQueueDma {
    common: NvmeOwnedQueueState,
    queue: HardwareQueue,
}

struct NvmeOwnedQueueState {
    slot: usize,
    name: &'static str,
    dma_mask: u64,
    page_size: usize,
    depth: usize,
    interrupt_sources: IdList,
    requests: NvmeQueueState,
    completion_cache: CompletionCache,
    completion_fault: bool,
}

pub(in crate::block) struct NvmeOwnedQueueEvidenceProgress {
    pub(in crate::block) completed: usize,
    pub(in crate::block) retained: bool,
}

impl PreparedNvmeOwnedQueue {
    #[allow(clippy::too_many_arguments)]
    pub(in crate::block) fn new(
        slot: usize,
        depth: usize,
        name: &'static str,
        dma_mask: u64,
        page_size: usize,
        interrupt_sources: IdList,
        queue: HardwareQueue,
        prp_lists: Vec<CoherentArray<u64>>,
    ) -> Self {
        let hardware_qid = queue.qid;
        let reinitialize_info = NvmeQueueReinitializeInfo::from_queue(&queue);
        Self {
            slot,
            hardware_qid,
            reinitialize_info,
            dma: QueueDmaOwner::new(NvmeOwnedQueueDma {
                common: NvmeOwnedQueueState {
                    slot,
                    name,
                    dma_mask,
                    page_size,
                    depth,
                    interrupt_sources,
                    requests: NvmeQueueState::new(depth, prp_lists),
                    completion_cache: CompletionCache::new(depth + 1),
                    completion_fault: false,
                },
                queue,
            }),
        }
    }

    /// Moves the initialized SQ/CQ owner into its final maintenance domain.
    pub(in crate::block) fn into_owned(self) -> NvmeOwnedQueue {
        let Self {
            slot,
            hardware_qid,
            reinitialize_info: _,
            dma,
        } = self;
        NvmeOwnedQueue {
            slot,
            hardware_qid,
            dma,
            _not_sync: PhantomData,
        }
    }

    pub(in crate::block) fn reinitialize_info(&self) -> NvmeQueueReinitializeInfo {
        self.reinitialize_info
    }
}

impl NvmeOwnedQueue {
    pub(in crate::block) const fn slot(&self) -> usize {
        self.slot
    }

    pub(in crate::block) const fn hardware_qid(&self) -> u32 {
        self.hardware_qid
    }

    pub(in crate::block) fn bind_dma_owner(
        &mut self,
        controller_cookie: usize,
        active_epoch: rdif_block::ControllerEpoch,
    ) -> Result<(), BlkError> {
        self.dma.bind(controller_cookie, active_epoch)
    }

    fn queue_info(
        &self,
        namespace: Namespace,
        max_transfer_bytes: Option<usize>,
    ) -> Result<QueueInfo, BlkError> {
        let dma = self.dma.live().ok_or(BlkError::Offline)?;
        Ok(QueueInfo {
            id: dma.common.slot,
            device: device_info(dma.common.name, namespace),
            limits: limits(
                dma.common.dma_mask,
                dma.common.page_size,
                max_transfer_bytes,
                namespace,
                dma.common.depth,
            ),
            kind: QueueKind::Interrupt {
                sources: dma.common.interrupt_sources,
            },
            execution: QueueExecution::Tagged,
        })
    }

    /// Installs request identity and DMA ownership before ringing the SQ
    /// doorbell. A rejected request was never made hardware-visible.
    pub(in crate::block) fn submit_owned(
        &mut self,
        namespace: Namespace,
        max_transfer_bytes: Option<usize>,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<AcceptedInterruptRequest, UnacceptedRequest> {
        let info = match self.queue_info(namespace, max_transfer_bytes) {
            Ok(info) => info,
            Err(error) => return Err(UnacceptedRequest::new(id, error, request)),
        };
        if let Err(error) = validate_owned_request(info, &request) {
            return Err(UnacceptedRequest::new(id, error, request));
        }
        self.submit_prepared(namespace, id, request)
            .map(|()| AcceptedInterruptRequest::new(id))
            .map_err(SubmitError::into_unaccepted)
    }

    fn submit_prepared(
        &mut self,
        namespace: Namespace,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<(), SubmitError> {
        let dma_owner = match self.dma.live_mut() {
            Some(dma_owner) => dma_owner,
            None => return Err(SubmitError::new(id, BlkError::Offline, request)),
        };
        let (mut request, mut prepared) = prepare_request_dma(id, request)?;
        let identity = match dma_owner.common.requests.alloc_identity() {
            Ok(identity) => identity,
            Err(error) => {
                request = restore_prepared_dma(request, prepared.take());
                return Err(SubmitError::new(id, owned_allocation_error(error), request));
            }
        };
        let (command, prp_list) = match dma_owner.common.requests.build_command(
            namespace,
            dma_owner.common.page_size,
            identity,
            &request,
            prepared.as_ref(),
        ) {
            Ok(command) => command,
            Err(error) => {
                dma_owner.common.requests.release_unaccepted(identity);
                request = restore_prepared_dma(request, prepared.take());
                return Err(SubmitError::new(id, owned_submit_error(error), request));
            }
        };
        let dma = prepared
            .take()
            // SAFETY: request identity and ownership are installed below
            // before the SQ tail doorbell makes the descriptor visible. Only a
            // matching CQE or proof-gated reclaim can return this DMA buffer.
            .map(|prepared| unsafe { prepared.into_in_flight() });
        dma_owner
            .common
            .requests
            .accept(identity, AcceptedRequest { id, request, dma }, prp_list);

        // `submit_io_data` performs the device write barrier before publishing
        // the new SQ tail. Hard IRQ observes only CQ phase and never aliases
        // the request table above.
        dma_owner.queue.submit_io_data(command);
        Ok(())
    }

    pub(in crate::block) fn service_claimed_evidence(
        &mut self,
        budget: usize,
        sink: &mut dyn CompletionSink,
    ) -> Result<NvmeOwnedQueueEvidenceProgress, BlkError> {
        let dma_owner = self.dma.live_mut().ok_or(BlkError::Offline)?;
        if dma_owner.common.completion_fault {
            return Err(BlkError::Io);
        }
        if budget == 0 {
            return Ok(NvmeOwnedQueueEvidenceProgress {
                completed: 0,
                retained: true,
            });
        }

        let emitted = dma_owner.common.requests.emit_cached_completions(
            dma_owner.common.slot,
            &mut dma_owner.common.completion_cache,
            budget,
            sink,
        )?;
        let remaining = budget.saturating_sub(emitted);
        if remaining == 0 {
            return Ok(NvmeOwnedQueueEvidenceProgress {
                completed: emitted,
                retained: true,
            });
        }

        let drain = drain_owner_completions_to_cache(
            &dma_owner.queue,
            &mut dma_owner.common.completion_cache,
            remaining,
        );
        dma_owner.common.completion_fault |= drain.invalid;
        if dma_owner.common.completion_fault {
            return Err(BlkError::Io);
        }
        let emitted_after_drain = dma_owner.common.requests.emit_cached_completions(
            dma_owner.common.slot,
            &mut dma_owner.common.completion_cache,
            remaining,
            sink,
        )?;
        let completed = emitted.saturating_add(emitted_after_drain);
        Ok(NvmeOwnedQueueEvidenceProgress {
            completed,
            retained: drain.may_have_more
                || completed == budget
                || dma_owner.common.completion_cache.has_ready(),
        })
    }

    pub(in crate::block) fn validate_quiescence(
        &self,
        proof: &rdif_block::DmaQuiesced,
    ) -> Result<(), BlkError> {
        self.dma.validate_quiescence(proof)
    }

    pub(in crate::block) fn reclaim_requests_after_quiesce(
        &mut self,
        proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        self.dma.validate_quiescence(proof)?;
        let dma_owner = self.dma.live_mut().ok_or(BlkError::Offline)?;
        dma_owner.common.requests.cancel_all(sink);
        dma_owner.common.completion_cache.clear_after_quiesce();
        dma_owner.common.completion_fault = false;
        if dma_owner.common.requests.has_accepted() {
            return Err(BlkError::Io);
        }
        unsafe {
            // SAFETY: the caller supplied the exact controller-bound proof;
            // runtime has stopped DMA and synchronized every matching action.
            dma_owner.queue.reset_after_controller_disable();
        }
        if !dma_owner.common.requests.advance_cid_epoch_after_quiesce() {
            return Err(BlkError::QueueEpochExhausted);
        }
        dma_owner.common.completion_cache.clear_after_quiesce();
        dma_owner.common.completion_fault = false;
        self.dma.record_quiesced(proof)?;
        Ok(())
    }

    pub(in crate::block) fn resume_after_reinitialize(
        &mut self,
        epoch: rdif_block::ControllerEpoch,
    ) -> Result<(), BlkError> {
        self.dma.resume_after_reinitialize(epoch)
    }

    pub(in crate::block) fn validate_resume(
        &self,
        epoch: rdif_block::ControllerEpoch,
    ) -> Result<(), BlkError> {
        self.dma.validate_resume(epoch)
    }

    pub(in crate::block) fn validate_shutdown(
        &self,
        epoch: rdif_block::ControllerEpoch,
    ) -> Result<(), BlkError> {
        let dma_owner = self.dma.live().ok_or(BlkError::Offline)?;
        if dma_owner.common.requests.has_accepted() || dma_owner.common.completion_cache.has_ready()
        {
            return Err(BlkError::Busy);
        }
        if dma_owner.common.completion_fault {
            return Err(BlkError::Io);
        }
        self.dma.validate_close(epoch)
    }

    pub(in crate::block) fn shutdown(
        &mut self,
        epoch: rdif_block::ControllerEpoch,
    ) -> Result<(), BlkError> {
        self.validate_shutdown(epoch)?;
        self.dma.close_after_quiesce(epoch)
    }
}

const fn owned_allocation_error(error: CidAllocationError) -> BlkError {
    match error {
        // The runtime owns the exact hardware-credit count. Reaching this case
        // after it granted a credit means the realized depth contract diverged;
        // it is not a transient device Busy condition.
        CidAllocationError::NoFreeSlot => BlkError::Io,
        CidAllocationError::GenerationExhausted => BlkError::QueueEpochExhausted,
    }
}

const fn owned_submit_error(error: BlkError) -> BlkError {
    match error {
        // Exact queue depth reserves one CID and PRP owner for every runtime
        // credit. A transient resource result after the credit was granted is
        // therefore a driver/runtime topology mismatch, not a retry signal.
        BlkError::Busy | BlkError::Retry => BlkError::Io,
        error => error,
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use rdif_block::{BlkError, ControllerEpoch, DmaQuiesced};

    use super::{NvmeOwnedQueue, QueueDmaOwner, owned_submit_error};

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn assert_send<T: Send>() {}

    #[test]
    fn final_queue_owner_can_move_to_exactly_one_maintenance_thread() {
        assert_send::<NvmeOwnedQueue>();
    }

    #[test]
    fn granted_credit_cannot_degrade_to_a_transient_driver_retry() {
        assert_eq!(owned_submit_error(BlkError::Retry), BlkError::Io);
        assert_eq!(owned_submit_error(BlkError::Busy), BlkError::Io);
        assert_eq!(
            owned_submit_error(BlkError::NotSupported),
            BlkError::NotSupported
        );
    }

    #[test]
    fn dropping_live_queue_without_quiescence_does_not_release_dma() {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = QueueDmaOwner::new(DropProbe(Arc::clone(&drops)));

        drop(owner);

        assert_eq!(drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn close_failure_retains_live_queue_dma_owner() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut owner = QueueDmaOwner::new(DropProbe(Arc::clone(&drops)));
        owner
            .bind(0x51a7, ControllerEpoch::INITIAL)
            .expect("fresh queue owner must bind once");

        assert_eq!(
            owner.close_after_quiesce(ControllerEpoch::new(2)),
            Err(BlkError::InvalidDmaProof)
        );
        assert!(owner.is_live());
        drop(owner);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn only_matching_quiescence_epoch_allows_dma_release() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut owner = QueueDmaOwner::new(DropProbe(Arc::clone(&drops)));
        owner
            .bind(0x51a7, ControllerEpoch::INITIAL)
            .expect("fresh queue owner must bind once");
        let wrong_owner = unsafe {
            // SAFETY: this value-only test has no hardware and observes only
            // the queue-owner proof classifier.
            DmaQuiesced::new(ControllerEpoch::new(2), 0xdead)
        };
        assert_eq!(
            owner.record_quiesced(&wrong_owner),
            Err(BlkError::InvalidDmaProof)
        );
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        let matching = unsafe {
            // SAFETY: the drop probe is not device-visible DMA; this proof is
            // used only to exercise exact cookie/epoch binding.
            DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7)
        };
        owner.record_quiesced(&matching).unwrap();
        owner.close_after_quiesce(ControllerEpoch::new(2)).unwrap();

        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn reinitialized_queue_requires_a_new_quiescence_proof_before_close() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut owner = QueueDmaOwner::new(DropProbe(Arc::clone(&drops)));
        owner
            .bind(0x51a7, ControllerEpoch::INITIAL)
            .expect("fresh queue owner must bind once");
        let recovery = unsafe {
            // SAFETY: the value-only drop probe is not reachable by hardware.
            DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7)
        };
        owner.record_quiesced(&recovery).unwrap();
        owner
            .resume_after_reinitialize(ControllerEpoch::new(2))
            .unwrap();

        assert_eq!(
            owner.close_after_quiesce(ControllerEpoch::new(2)),
            Err(BlkError::InvalidDmaProof)
        );
        drop(owner);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
    }
}
