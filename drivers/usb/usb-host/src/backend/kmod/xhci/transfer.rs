use alloc::{
    collections::{BTreeMap, btree_map::Entry},
    sync::Arc,
};

use xhci::ring::trb::event::{CompletionCode, TransferEvent};

use super::{reg::XhciRegistersShared, ring::SendRing, sync::IrqLock};
use crate::{BusAddr, queue::Finished, usb_if::err::TransferError};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransferId(pub(crate) BusAddr);

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Clone, Copy)]
pub struct TransQueueId {
    slot_id: u8,
    ep_id: u8,
}

#[derive(Clone)]
pub struct TransferResultHandler {
    inner: Arc<IrqLock<BTreeMap<TransQueueId, Finished<TransferEvent>>>>,
}

impl TransferResultHandler {
    pub fn new(reg: XhciRegistersShared) -> Self {
        Self {
            inner: Arc::new(IrqLock::new(BTreeMap::new(), reg)),
        }
    }

    pub fn register_queue(
        &self,
        slot_id: u8,
        ep_id: u8,
        ring: &SendRing<TransferEvent>,
    ) -> Result<(), TransferError> {
        let id = TransQueueId { slot_id, ep_id };
        let handle = ring.finished_handle();
        match self.inner.lock().entry(id) {
            Entry::Vacant(entry) => {
                entry.insert(handle);
                Ok(())
            }
            Entry::Occupied(_) => Err(TransferError::QueueFull),
        }
    }

    /// Atomically replaces all completion routes participating in one endpoint
    /// configuration transaction.
    pub fn replace_queues(
        &self,
        slot_id: u8,
        old: impl Iterator<Item = (u8, Finished<TransferEvent>)>,
        new: impl Iterator<Item = (u8, Finished<TransferEvent>)>,
    ) -> Result<(), TransferError> {
        let old = old
            .map(|(ep_id, queue)| (TransQueueId { slot_id, ep_id }, queue))
            .collect::<BTreeMap<_, _>>();
        let new = new
            .map(|(ep_id, queue)| (TransQueueId { slot_id, ep_id }, queue))
            .collect::<BTreeMap<_, _>>();
        let mut queues = self.inner.lock();

        for (id, expected) in &old {
            if !queues
                .get(id)
                .is_some_and(|active| active.same_queue(expected))
            {
                return Err(TransferError::InvalidEndpoint);
            }
        }
        for (id, pending) in &new {
            if queues
                .get(id)
                .is_some_and(|active| !old.contains_key(id) && !active.same_queue(pending))
            {
                return Err(TransferError::QueueFull);
            }
        }

        for id in old.keys() {
            queues.remove(id);
        }
        queues.extend(new);
        Ok(())
    }

    /// Marks a queue completion from the xHCI interrupt path.
    ///
    /// This runs while handling an interrupt, so it must not acquire OS-facing
    /// locks or call into device/file managers. Queue registration is protected
    /// by `IrqLock::lock`, which disables this interrupt source before mutating
    /// the map. The IRQ hot path uses `force_use` and only touches the
    /// pre-registered queue completion slot, then wakes queue-local waiters.
    pub unsafe fn set_finished(
        &self,
        slot_id: u8,
        ep_id: u8,
        ptr: BusAddr,
        res: TransferEvent,
    ) -> bool {
        // xHCI reports ISO ring underrun/overrun when the periodic ring is
        // empty. Linux treats these as ring xrun events, not TD completions.
        if is_iso_ring_xrun(res) {
            trace!(
                "xhci: ignore ISO ring xrun event slot={} ep={} ptr={:#x} code={:?}",
                slot_id,
                ep_id,
                ptr.raw(),
                res.completion_code()
            );
            return true;
        }

        let queue_id = TransQueueId { slot_id, ep_id };
        if let Some(q) = unsafe { self.inner.force_use().get(&queue_id) } {
            trace!(
                "xhci: dispatch transfer event slot={} ep={} ptr={:#x} code={:?} len={}",
                slot_id,
                ep_id,
                ptr.raw(),
                res.completion_code(),
                res.trb_transfer_length()
            );
            if q.try_set_finished(ptr, res) {
                true
            } else {
                error!(
                    "xhci: controller fault: transfer event points outside active ring slot={} \
                     ep={} ptr={:#x}",
                    slot_id,
                    ep_id,
                    ptr.raw()
                );
                false
            }
        } else {
            error!(
                "xhci: controller fault: transfer event has no active endpoint route slot={} \
                 ep={} ptr={:#x} code={:?} len={}",
                slot_id,
                ep_id,
                ptr.raw(),
                res.completion_code(),
                res.trb_transfer_length()
            );
            false
        }
    }
}

fn is_iso_ring_xrun(event: TransferEvent) -> bool {
    matches!(
        event.completion_code(),
        Ok(CompletionCode::RingUnderrun | CompletionCode::RingOverrun)
    )
}
