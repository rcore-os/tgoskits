//! Accepted-request ownership, CID slots, and terminal completion.

use alloc::vec::Vec;
use core::num::NonZeroU16;

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
    cid_epoch: u64,
}

pub(in crate::block) struct RequestSlot {
    pub(in crate::block) state: SlotState,
    active_identity: Option<CommandIdentity>,
    cid_generation: u16,
    accepted: Option<AcceptedRequest>,
    prp_list: Option<CoherentArray<u64>>,
}

/// Hardware-visible command identity within one DMA-quiesced queue epoch.
///
/// The low bits select the bounded request slot. The remaining bits carry a
/// generation fragment, so a late CQE cannot claim a request that reused the
/// same slot. Generation wrap is forbidden until the queue has completed a
/// proof-gated epoch transition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::block) struct CommandIdentity(NonZeroU16);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CidAllocationError {
    NoFreeSlot,
    GenerationExhausted,
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
            active_identity: None,
            cid_generation: 0,
            accepted: None,
            prp_list: None,
        });
        Self {
            slots,
            free_cids: (1..=depth).rev().collect(),
            free_prp_lists: prp_lists,
            cid_epoch: 1,
        }
    }

    pub(super) fn has_accepted(&self) -> bool {
        self.slots.iter().any(|slot| slot.accepted.is_some())
    }

    pub(super) fn alloc_identity(&mut self) -> Result<CommandIdentity, CidAllocationError> {
        if self.free_cids.is_empty() {
            return Err(CidAllocationError::NoFreeSlot);
        }
        let Some(free_index) = self
            .free_cids
            .iter()
            .rposition(|slot| self.slots[*slot].cid_generation < CommandIdentity::MAX_GENERATION)
        else {
            return Err(CidAllocationError::GenerationExhausted);
        };
        let slot = self.free_cids[free_index];
        let Some(generation) = self.slots[slot].cid_generation.checked_add(1) else {
            return Err(CidAllocationError::GenerationExhausted);
        };
        let Some(identity) = CommandIdentity::new(slot, generation) else {
            return Err(CidAllocationError::GenerationExhausted);
        };
        self.free_cids.swap_remove(free_index);
        self.slots[slot].cid_generation = generation;
        Ok(identity)
    }

    pub(super) fn release_unaccepted(&mut self, identity: CommandIdentity) {
        let cid = identity.slot();
        let Some(slot) = self.slots.get_mut(cid) else {
            return;
        };
        debug_assert!(slot.accepted.is_none());
        debug_assert_eq!(slot.cid_generation, identity.generation());
        debug_assert!(slot.active_identity.is_none());
        if let Some(prp_list) = slot.prp_list.take() {
            self.free_prp_lists.push(prp_list);
        }
        slot.state = SlotState::Free;
        self.free_cids.push(cid);
    }

    pub(super) fn accept(
        &mut self,
        identity: CommandIdentity,
        request: AcceptedRequest,
        prp_list: Option<CoherentArray<u64>>,
    ) {
        let cid = identity.slot();
        let slot = &mut self.slots[cid];
        debug_assert_eq!(slot.state, SlotState::Free);
        debug_assert!(slot.accepted.is_none());
        debug_assert_eq!(slot.cid_generation, identity.generation());
        slot.state = SlotState::Pending;
        slot.active_identity = Some(identity);
        slot.accepted = Some(request);
        slot.prp_list = prp_list;
    }

    pub(super) fn build_command(
        &mut self,
        namespace: Namespace,
        page_size: usize,
        identity: CommandIdentity,
        request: &OwnedRequest,
        dma: Option<&PreparedDma>,
    ) -> Result<(CommandSet, Option<CoherentArray<u64>>), BlkError> {
        let cid = identity.raw();
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
                    RequestOp::Flush | RequestOp::Discard | RequestOp::WriteZeroes => {
                        return Err(BlkError::InvalidRequest);
                    }
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
        self.validate_ready_completions(cache, ready)?;

        let mut emitted = 0;
        for cid in 1..self.slots.len() {
            if emitted == budget {
                break;
            }
            if !ready.contains(cid) {
                continue;
            }
            let Some(completion) = cache.take(cid) else {
                continue;
            };
            let slot = &mut self.slots[cid];
            if slot.state != SlotState::Pending {
                return Err(BlkError::Io);
            }
            let accepted = slot.accepted.take().ok_or(BlkError::Io)?;
            let result = if completion.status.success {
                Ok(())
            } else {
                warn!(
                    "nvme queue {} command {:#x} failed: status={:#x}, result={:#x}",
                    queue_id,
                    completion.identity.raw(),
                    completion.status.raw_status,
                    completion.status.result
                );
                Err(BlkError::Io)
            };
            if let Some(prp_list) = slot.prp_list.take() {
                self.free_prp_lists.push(prp_list);
            }
            slot.state = SlotState::Free;
            slot.active_identity = None;
            self.free_cids.push(cid);
            // SAFETY: the matching CQ entry was consumed above, so the NVMe
            // controller has relinquished this command's DMA backing.
            let completion = unsafe { accepted.complete_after_quiesce(result) };
            sink.complete(completion);
            emitted += 1;
        }
        Ok(emitted)
    }

    fn validate_ready_completions(
        &self,
        cache: &CompletionCache,
        ready: ReadyCompletionSnapshot,
    ) -> Result<(), BlkError> {
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
            let Some(completion) = cache.get(cid) else {
                return Err(BlkError::Io);
            };
            if slot.active_identity != Some(completion.identity) {
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
            slot.active_identity = None;
            self.free_cids.push(cid);
            // SAFETY: proof-gated reclaim requires IRQ synchronization and DMA
            // quiescence before entering this path.
            let completion = unsafe { accepted.complete_after_quiesce(Err(BlkError::Cancelled)) };
            sink.complete(completion);
        }
    }

    /// Begins a fresh hardware-CID namespace after the controller, CQ, IRQ
    /// action, and DMA engine have all been proved quiescent.
    pub(super) fn advance_cid_epoch_after_quiesce(&mut self) -> bool {
        let Some(next_epoch) = self.cid_epoch.checked_add(1) else {
            return false;
        };
        debug_assert!(!self.has_accepted());
        self.cid_epoch = next_epoch;
        self.free_cids.clear();
        for cid in (1..self.slots.len()).rev() {
            let slot = &mut self.slots[cid];
            debug_assert!(slot.prp_list.is_none());
            slot.state = SlotState::Free;
            slot.active_identity = None;
            slot.cid_generation = 0;
            self.free_cids.push(cid);
        }
        true
    }
}

impl CommandIdentity {
    const SLOT_BITS: u32 = 7;
    const SLOT_MASK: u16 = (1_u16 << Self::SLOT_BITS) - 1;
    const MAX_SLOT: usize = ReadyCompletionSnapshot::MAX_CID;
    pub(in crate::block) const MAX_GENERATION: u16 = (1_u16 << (u16::BITS - Self::SLOT_BITS)) - 1;

    pub(in crate::block) const fn new(slot: usize, generation: u16) -> Option<Self> {
        if slot == 0
            || slot > Self::MAX_SLOT
            || generation == 0
            || generation > Self::MAX_GENERATION
        {
            return None;
        }
        let raw = (generation << Self::SLOT_BITS) | slot as u16;
        match NonZeroU16::new(raw) {
            Some(raw) => Some(Self(raw)),
            None => None,
        }
    }

    pub(in crate::block) fn from_raw(raw: u16) -> Option<Self> {
        let generation = raw >> Self::SLOT_BITS;
        let slot = usize::from(raw & Self::SLOT_MASK);
        Self::new(slot, generation)
    }

    pub(in crate::block) const fn raw(self) -> u16 {
        self.0.get()
    }

    pub(in crate::block) const fn slot(self) -> usize {
        (self.raw() & Self::SLOT_MASK) as usize
    }

    pub(in crate::block) const fn generation(self) -> u16 {
        self.raw() >> Self::SLOT_BITS
    }

    #[cfg(test)]
    pub(in crate::block) const fn new_for_test(slot: usize, generation: u16) -> Self {
        match Self::new(slot, generation) {
            Some(identity) => identity,
            None => panic!("invalid test command identity"),
        }
    }
}

impl CidAllocationError {
    pub(super) const fn into_block_error(self) -> BlkError {
        match self {
            Self::NoFreeSlot => BlkError::Retry,
            Self::GenerationExhausted => BlkError::QueueEpochExhausted,
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
mod tests;
