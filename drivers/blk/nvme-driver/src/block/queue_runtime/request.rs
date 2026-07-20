//! Accepted-request ownership, CID slots, and terminal completion.

use alloc::vec::Vec;

use dma_api::{CoherentArray, InFlightDma, PreparedDma};
use log::warn;
#[cfg(test)]
use rdif_block::RequestFlags;
use rdif_block::{BlkError, CompletedRequest, CompletionSink, OwnedRequest, RequestId, RequestOp};

use super::prp::build_prp_mapping;
use crate::{
    Namespace,
    block::completion::{CompletionCache, ReadyCompletionSnapshot},
    queue::CommandSet,
};

pub(super) struct NvmeQueueState {
    slots: Vec<RequestSlot>,
    free_cids: Vec<usize>,
    free_prp_lists: Vec<CoherentArray<u64>>,
}

pub(in crate::block) struct RequestSlot {
    pub(in crate::block) state: SlotState,
    accepted: Option<AcceptedRequest>,
    prp_list: Option<CoherentArray<u64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::block) enum SlotState {
    Free,
    Pending,
}

pub(in crate::block) struct AcceptedRequest {
    pub(in crate::block) id: RequestId,
    pub(in crate::block) request: OwnedRequest,
    pub(in crate::block) dma: Option<InFlightDma>,
}

impl NvmeQueueState {
    pub(super) fn new(depth: usize, prp_lists: Vec<CoherentArray<u64>>) -> Self {
        let mut slots = Vec::with_capacity(depth + 1);
        slots.resize_with(depth + 1, || RequestSlot {
            state: SlotState::Free,
            accepted: None,
            prp_list: None,
        });
        Self {
            slots,
            free_cids: (1..=depth).rev().collect(),
            free_prp_lists: prp_lists,
        }
    }

    pub(super) fn has_accepted(&self) -> bool {
        self.slots.iter().any(|slot| slot.accepted.is_some())
    }

    pub(super) fn alloc_cid(&mut self) -> Result<usize, BlkError> {
        self.free_cids.pop().ok_or(BlkError::Retry)
    }

    pub(super) fn release_cid(&mut self, cid: usize) {
        let Some(slot) = self.slots.get_mut(cid) else {
            return;
        };
        debug_assert!(slot.accepted.is_none());
        if let Some(prp_list) = slot.prp_list.take() {
            self.free_prp_lists.push(prp_list);
        }
        slot.state = SlotState::Free;
        self.free_cids.push(cid);
    }

    pub(super) fn accept(
        &mut self,
        cid: usize,
        request: AcceptedRequest,
        prp_list: Option<CoherentArray<u64>>,
    ) {
        let slot = &mut self.slots[cid];
        debug_assert_eq!(slot.state, SlotState::Free);
        debug_assert!(slot.accepted.is_none());
        slot.state = SlotState::Pending;
        slot.accepted = Some(request);
        slot.prp_list = prp_list;
    }

    pub(super) fn build_command(
        &mut self,
        namespace: Namespace,
        page_size: usize,
        cid: usize,
        request: &OwnedRequest,
        dma: Option<&PreparedDma>,
    ) -> Result<(CommandSet, Option<CoherentArray<u64>>), BlkError> {
        let cid = u16::try_from(cid).map_err(|_| BlkError::InvalidRequest)?;
        match request.op {
            RequestOp::Read | RequestOp::Write => {
                let dma = dma.ok_or(BlkError::InvalidRequest)?;
                let prp = build_prp_mapping(&mut self.free_prp_lists, page_size, dma)?;
                let command = match request.op {
                    RequestOp::Read => CommandSet::nvm_cmd_read_with_cid(
                        namespace.id,
                        prp.prp1,
                        prp.prp2,
                        request.lba,
                        request.block_count,
                        cid,
                    ),
                    RequestOp::Write => CommandSet::nvm_cmd_write_with_cid(
                        namespace.id,
                        prp.prp1,
                        prp.prp2,
                        request.lba,
                        request.block_count,
                        cid,
                    ),
                    _ => unreachable!(),
                };
                Ok((command, prp.prp_list))
            }
            RequestOp::Flush => Ok((CommandSet::nvm_cmd_flush_with_cid(namespace.id, cid), None)),
            RequestOp::Discard | RequestOp::WriteZeroes => Err(BlkError::NotSupported),
        }
    }

    pub(super) fn emit_cached_completions(
        &mut self,
        queue_id: usize,
        cache: &mut CompletionCache,
        budget: usize,
        sink: &mut dyn CompletionSink,
    ) -> Result<usize, BlkError> {
        let ready = cache.ready_snapshot();
        self.validate_ready_completions(ready)?;

        let mut emitted = 0;
        for cid in 1..self.slots.len() {
            if emitted == budget {
                break;
            }
            if !ready.contains(cid) {
                continue;
            }
            let Some(status) = cache.take(cid) else {
                continue;
            };
            let slot = &mut self.slots[cid];
            if slot.state != SlotState::Pending {
                return Err(BlkError::Io);
            }
            let accepted = slot.accepted.take().ok_or(BlkError::Io)?;
            let result = if status.success {
                Ok(())
            } else {
                warn!(
                    "nvme queue {} command {} failed: status={:#x}, result={:#x}",
                    queue_id, cid, status.raw_status, status.result
                );
                Err(BlkError::Io)
            };
            if let Some(prp_list) = slot.prp_list.take() {
                self.free_prp_lists.push(prp_list);
            }
            slot.state = SlotState::Free;
            self.free_cids.push(cid);
            // SAFETY: the matching CQ entry was consumed above, so the NVMe
            // controller has relinquished this command's DMA backing.
            let completion = unsafe { accepted.complete_after_quiesce(result) };
            sink.complete(completion);
            emitted += 1;
        }
        Ok(emitted)
    }

    fn validate_ready_completions(&self, ready: ReadyCompletionSnapshot) -> Result<(), BlkError> {
        for cid in 1..=ReadyCompletionSnapshot::MAX_CID {
            if !ready.contains(cid) {
                continue;
            }
            let Some(slot) = self.slots.get(cid) else {
                return Err(BlkError::Io);
            };
            if slot.state != SlotState::Pending || slot.accepted.is_none() {
                return Err(BlkError::Io);
            }
        }
        Ok(())
    }

    pub(super) fn cancel_all(&mut self, sink: &mut dyn CompletionSink) {
        for cid in 1..self.slots.len() {
            let slot = &mut self.slots[cid];
            let Some(accepted) = slot.accepted.take() else {
                continue;
            };
            if let Some(prp_list) = slot.prp_list.take() {
                self.free_prp_lists.push(prp_list);
            }
            slot.state = SlotState::Free;
            self.free_cids.push(cid);
            // SAFETY: proof-gated reclaim requires IRQ synchronization and DMA
            // quiescence before entering this path.
            let completion = unsafe { accepted.complete_after_quiesce(Err(BlkError::Cancelled)) };
            sink.complete(completion);
        }
    }
}

impl AcceptedRequest {
    /// Restores CPU ownership and builds the terminal runtime completion.
    ///
    /// # Safety
    ///
    /// The matching NVMe command must have completed, or the controller and
    /// queue must be quiesced so no bus-master access to `self.dma` is possible.
    pub(in crate::block) unsafe fn complete_after_quiesce(
        self,
        result: Result<(), BlkError>,
    ) -> CompletedRequest {
        let Self {
            id,
            mut request,
            dma,
        } = self;
        if let Some(dma) = dma {
            // SAFETY: upheld by the method contract above.
            let completed = unsafe { dma.complete_after_quiesce() };
            request.data = Some(completed.into_cpu_buffer());
        }
        CompletedRequest::new(id, result, request)
    }
}

#[cfg(test)]
impl RequestSlot {
    pub(in crate::block) fn pending_for_test(id: RequestId) -> Self {
        Self {
            state: SlotState::Pending,
            accepted: Some(AcceptedRequest {
                id,
                request: OwnedRequest {
                    op: RequestOp::Flush,
                    lba: 0,
                    block_count: 0,
                    data: None,
                    flags: RequestFlags::NONE,
                },
                dma: None,
            }),
            prp_list: None,
        }
    }

    pub(in crate::block) fn runtime_id(&self) -> Option<RequestId> {
        self.accepted.as_ref().map(|request| request.id)
    }
}

#[cfg(test)]
mod tests {
    use alloc::vec::Vec;

    use rdif_block::{CompletedRequest, CompletionSink};

    use super::*;
    use crate::block::{CachedCompletion, CompletionStatus};

    #[derive(Default)]
    struct CountingSink {
        completions: Vec<CompletedRequest>,
    }

    impl CompletionSink for CountingSink {
        fn complete(&mut self, completion: CompletedRequest) {
            self.completions.push(completion);
        }
    }

    #[test]
    fn irq_completion_then_recovery_reclaim_emits_one_terminal_result() {
        let runtime_id = RequestId::new(0x55aa);
        let mut state = NvmeQueueState::new(1, Vec::new());
        let cid = state.alloc_cid().expect("one CID must be available");
        state.accept(cid, accepted_flush(runtime_id), None);
        let mut cache = CompletionCache::new(2);
        assert!(cache.record(CachedCompletion {
            cid,
            status: CompletionStatus {
                success: true,
                raw_status: 0,
                result: 0,
            },
        }));
        let mut sink = CountingSink::default();

        assert_eq!(
            state
                .emit_cached_completions(0, &mut cache, 64, &mut sink)
                .expect("valid CQE must complete its accepted request"),
            1
        );
        state.cancel_all(&mut sink);

        assert_eq!(sink.completions.len(), 1);
        assert_eq!(sink.completions[0].id, runtime_id);
        assert_eq!(sink.completions[0].result, Ok(()));
        assert!(!state.has_accepted());
    }

    #[test]
    fn repeated_quiesced_reclaim_cancels_each_accepted_request_once() {
        let runtime_id = RequestId::new(7);
        let mut state = NvmeQueueState::new(1, Vec::new());
        let cid = state.alloc_cid().expect("one CID must be available");
        state.accept(cid, accepted_flush(runtime_id), None);
        let mut sink = CountingSink::default();

        state.cancel_all(&mut sink);
        state.cancel_all(&mut sink);

        assert_eq!(sink.completions.len(), 1);
        assert_eq!(sink.completions[0].id, runtime_id);
        assert_eq!(sink.completions[0].result, Err(BlkError::Cancelled));
        assert!(!state.has_accepted());
    }

    #[test]
    fn invalid_cached_cid_prevents_all_terminal_publication_in_the_batch() {
        let runtime_id = RequestId::new(9);
        let mut state = NvmeQueueState::new(2, Vec::new());
        let cid = state.alloc_cid().expect("one CID must be available");
        assert_eq!(cid, 1);
        state.accept(cid, accepted_flush(runtime_id), None);
        let mut cache = CompletionCache::new(3);
        assert!(cache.record(CachedCompletion {
            cid,
            status: CompletionStatus {
                success: true,
                raw_status: 0,
                result: 0,
            },
        }));
        assert!(cache.record(CachedCompletion {
            cid: 2,
            status: CompletionStatus {
                success: false,
                raw_status: 0xdead,
                result: 0,
            },
        }));
        let mut sink = CountingSink::default();

        assert_eq!(
            state.emit_cached_completions(0, &mut cache, 64, &mut sink),
            Err(BlkError::Io)
        );
        assert!(
            sink.completions.is_empty(),
            "queue corruption must be diagnosed before any completion callback"
        );
        assert!(state.has_accepted());
    }

    fn accepted_flush(id: RequestId) -> AcceptedRequest {
        AcceptedRequest {
            id,
            request: OwnedRequest {
                op: RequestOp::Flush,
                lba: 0,
                block_count: 0,
                data: None,
                flags: RequestFlags::NONE,
            },
            dma: None,
        }
    }
}
