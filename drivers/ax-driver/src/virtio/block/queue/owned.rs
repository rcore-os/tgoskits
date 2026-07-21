//! Queue ring, doorbell, and DMA requests owned by one v0.13 domain.

use alloc::boxed::Box;
use core::{fmt, mem, mem::ManuallyDrop, ptr};

use rdif_block::{
    AcceptedRequest, BlkError, CompletedRequest, CompletionSink, ControllerEpoch, DmaQuiesced,
    DriverDeviceKey, QueueInfo, RequestId, UnacceptedRequest,
};
use virtio_drivers::queue::VirtQueue;

use super::{
    InflightRequest, InflightStorage, ReclaimProofTracker, VIRTIO_BLK_QUEUE_SIZE,
    add_read_descriptor, add_write_descriptor, complete_consumed_inflight,
    map_virtio_completion_err_to_blk_err, map_virtio_err_to_blk_err, pop_used_descriptor,
    prepare_virtio_dma, take_inflight_after_used_descriptor, virtio_queue_info,
};
use crate::virtio::{
    VirtIoTransport,
    block::{
        VIRTIO_BLK_QUEUE_ID,
        device::VirtIoBlkInner,
        notify::{BoundVirtioQueueNotifyPort, VirtioQueueNotifyPort},
    },
};

pub(in crate::virtio::block) struct VirtioOwnedQueue {
    dma: QueueDmaOwner<VirtioOwnedQueueDma>,
    device_key: DriverDeviceKey,
    info: QueueInfo,
    reclaim_proof: ReclaimProofTracker,
}

struct VirtioOwnedQueueDma {
    queue: VirtQueue<crate::virtio::VirtIoHalImpl, VIRTIO_BLK_QUEUE_SIZE>,
    notify: BoundVirtioQueueNotifyPort,
    descriptor_storage: Box<InflightStorage>,
    inflight: Option<InflightRequest>,
}

enum QueueDmaOwner<T> {
    Live(LiveQueueDmaOwner<T>),
    Closed,
    Quarantined(QuarantinedQueueDmaOwner<T>),
}

struct LiveQueueDmaOwner<T> {
    storage: ManuallyDrop<T>,
    controller_cookie: usize,
    active_epoch: ControllerEpoch,
    quiesced_epoch: Option<ControllerEpoch>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum QueueDmaQuarantineReason {
    DroppedWithoutQuiescence,
}

/// Named retention of a VirtIO ring whose reset proof was not consumed.
struct QuarantinedQueueDmaOwner<T> {
    storage: ManuallyDrop<T>,
    controller_cookie: usize,
    active_epoch: ControllerEpoch,
    reason: QueueDmaQuarantineReason,
}

impl VirtioOwnedQueue {
    pub(in crate::virtio::block) fn take_ready<T: VirtIoTransport>(
        inner: &mut VirtIoBlkInner<T>,
        notify: VirtioQueueNotifyPort,
        device_key: DriverDeviceKey,
        controller_cookie: usize,
        queue_epoch: ControllerEpoch,
        interrupts_enabled: bool,
    ) -> Result<Self, (BlkError, VirtioQueueNotifyPort)> {
        if inner.init_phase != crate::virtio::block::initialization::VirtioBlockInitPhase::Ready
            || inner.inflight.is_some()
            || inner.queue.is_none()
            || inner.descriptor_storage.is_none()
        {
            return Err((BlkError::Offline, notify));
        }
        let Some(mut queue) = inner.queue.take() else {
            return Err((BlkError::Offline, notify));
        };
        let Some(descriptor_storage) = inner.descriptor_storage.take() else {
            inner.queue = Some(queue);
            return Err((BlkError::Offline, notify));
        };
        let notify = match notify.bind_queue(VIRTIO_BLK_QUEUE_ID as u16) {
            Ok(notify) => notify,
            Err((error, notify)) => {
                inner.queue = Some(queue);
                inner.descriptor_storage = Some(descriptor_storage);
                return Err((error, notify));
            }
        };
        let mut info = virtio_queue_info(inner.capacity);
        info.device.read_only =
            inner.negotiated_features & crate::virtio::block::initialization::VIRTIO_BLK_F_RO != 0;
        queue.set_dev_notify(interrupts_enabled);
        Ok(Self {
            dma: QueueDmaOwner::new(
                VirtioOwnedQueueDma {
                    queue,
                    notify,
                    descriptor_storage,
                    inflight: None,
                },
                controller_cookie,
                queue_epoch,
            ),
            device_key,
            info,
            reclaim_proof: ReclaimProofTracker {
                controller_cookie,
                last_epoch: Some(queue_epoch.get()),
            },
        })
    }

    pub(in crate::virtio::block) const fn info(&self) -> QueueInfo {
        self.info
    }

    pub(in crate::virtio::block) fn set_interrupts(&mut self, enabled: bool) {
        if let Some(dma) = self.dma.live_mut() {
            dma.queue.set_dev_notify(enabled);
        }
    }

    pub(in crate::virtio::block) fn submit_owned(
        &mut self,
        logical_device: DriverDeviceKey,
        id: RequestId,
        request: rdif_block::OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest> {
        if logical_device != self.device_key {
            return Err(UnacceptedRequest::new(
                id,
                BlkError::InvalidRequest,
                request,
            ));
        }
        if let Err(error) = rdif_block::validate_owned_request(self.info, &request) {
            return Err(UnacceptedRequest::new(id, error, request));
        }
        let Some(dma_owner) = self.dma.live_mut() else {
            return Err(UnacceptedRequest::new(id, BlkError::Offline, request));
        };
        if dma_owner.inflight.is_some() {
            // Discovery advertises one credit. This is a runtime contract
            // violation, and no descriptor has become hardware-visible.
            return Err(UnacceptedRequest::new(
                id,
                BlkError::Other("VirtIO hardware credit was not reserved"),
                request,
            ));
        }

        let (op, mut request, prepared) =
            prepare_virtio_dma(id, request).map_err(rdif_block::SubmitError::into_unaccepted)?;
        dma_owner.descriptor_storage.prepare(op, request.lba);
        let ptr = prepared.cpu_ptr();
        let len = prepared.len().get();
        let token = match op {
            super::InflightOp::Read => {
                // SAFETY: `prepared` owns a stable allocation until matching
                // used-ring consumption or DMA-quiesced recovery.
                let buffer = unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), len) };
                unsafe {
                    add_read_descriptor(
                        &mut dma_owner.queue,
                        &dma_owner.descriptor_storage.req,
                        buffer,
                        &mut dma_owner.descriptor_storage.resp,
                    )
                }
            }
            super::InflightOp::Write => {
                // SAFETY: the same allocation remains device-owned for the
                // complete descriptor lifetime.
                let buffer = unsafe { core::slice::from_raw_parts(ptr.as_ptr().cast_const(), len) };
                unsafe {
                    add_write_descriptor(
                        &mut dma_owner.queue,
                        &dma_owner.descriptor_storage.req,
                        buffer,
                        &mut dma_owner.descriptor_storage.resp,
                    )
                }
            }
        };
        let token = match token {
            Ok(token) => token,
            Err(error) => {
                request.data = Some(prepared.into_cpu_buffer());
                return Err(UnacceptedRequest::new(
                    id,
                    map_reserved_credit_error(error),
                    request,
                ));
            }
        };

        // SAFETY: the descriptor is visible and this exact allocation remains
        // retained until completion or controller quiescence.
        let request_dma = unsafe { prepared.into_in_flight() };
        dma_owner.inflight = Some(InflightRequest {
            id,
            token,
            op,
            request,
            dma: request_dma,
        });
        if dma_owner.queue.should_notify() {
            dma_owner.notify.notify();
        }
        Ok(AcceptedRequest::new(id))
    }

    pub(in crate::virtio::block) fn service_queue_fact(
        &mut self,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        let dma = self.dma.live_mut().ok_or(BlkError::Offline)?;
        let inflight = dma.inflight.as_ref().ok_or(BlkError::Io)?;
        let used_token = dma.queue.peek_used().ok_or(BlkError::Io)?;
        if used_token != inflight.token {
            return Err(BlkError::Io);
        }
        let inflight = take_inflight_after_used_descriptor(&mut dma.inflight, |inflight| {
            pop_used_descriptor(&mut dma.queue, &mut dma.descriptor_storage, inflight)
        })?;
        let result = super::virtio_response_result(dma.descriptor_storage.resp[0])
            .map_err(map_virtio_completion_err_to_blk_err);
        sink.complete(complete_consumed_inflight(inflight, result));
        Ok(())
    }

    pub(in crate::virtio::block) fn reclaim_after_quiesce(
        &mut self,
        proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        self.reclaim_proof.validate(proof)?;
        self.dma.validate_quiescence(proof)?;
        let dma = self.dma.live_mut().ok_or(BlkError::Offline)?;
        if let Some(mut inflight) = dma.inflight.take() {
            // SAFETY: the validated proof establishes that neither ring nor
            // request buffer remains reachable by hardware.
            let completed = unsafe { inflight.dma.complete_after_quiesce() };
            inflight.request.data = Some(completed.into_cpu_buffer());
            sink.complete(CompletedRequest::new(
                inflight.id,
                Err(BlkError::Cancelled),
                inflight.request,
            ));
        }
        self.dma.record_quiesced(proof)?;
        self.reclaim_proof.commit(proof);
        Ok(())
    }

    pub(in crate::virtio::block) fn into_reinitialize_parts(
        mut self,
        epoch: ControllerEpoch,
    ) -> Result<(VirtioQueueNotifyPort, Box<InflightStorage>), Box<Self>> {
        if self.dma.live().is_none_or(|dma| dma.inflight.is_some()) {
            return Err(Box::new(self));
        }
        let dma = match self.dma.take_after_quiesce(epoch) {
            Ok(dma) => dma,
            Err(_) => return Err(Box::new(self)),
        };
        let VirtioOwnedQueueDma {
            queue,
            notify,
            descriptor_storage,
            inflight,
        } = dma;
        debug_assert!(inflight.is_none());
        // The matching DmaQuiesced proof was validated before the queue owner
        // committed this epoch. The old ring can no longer be reached by the
        // device, so its DMA layout may be released before reconstruction.
        drop(queue);
        Ok((notify.into_unbound(), descriptor_storage))
    }

    pub(in crate::virtio::block) fn shutdown(
        &mut self,
        epoch: ControllerEpoch,
    ) -> Result<(), BlkError> {
        let dma = self.dma.live().ok_or(BlkError::Offline)?;
        if dma.inflight.is_some() {
            return Err(BlkError::Busy);
        }
        self.dma.close_after_quiesce(epoch)
    }
}

impl<T> QueueDmaOwner<T> {
    fn new(storage: T, controller_cookie: usize, active_epoch: ControllerEpoch) -> Self {
        Self::Live(LiveQueueDmaOwner {
            storage: ManuallyDrop::new(storage),
            controller_cookie,
            active_epoch,
            quiesced_epoch: None,
        })
    }

    fn live(&self) -> Option<&T> {
        match self {
            Self::Live(live) => Some(&live.storage),
            Self::Closed => None,
            Self::Quarantined(quarantine) => {
                let _retained_owner = quarantine;
                None
            }
        }
    }

    fn live_mut(&mut self) -> Option<&mut T> {
        match self {
            Self::Live(live) => Some(&mut live.storage),
            Self::Closed => None,
            Self::Quarantined(quarantine) => {
                let _retained_owner = quarantine;
                None
            }
        }
    }

    fn validate_quiescence(&self, proof: &DmaQuiesced) -> Result<(), BlkError> {
        let live = self.live_owner()?;
        if live.controller_cookie == 0
            || proof.controller_cookie() != live.controller_cookie
            || proof.epoch() <= live.active_epoch
            || live.quiesced_epoch.is_some()
        {
            return Err(BlkError::InvalidDmaProof);
        }
        Ok(())
    }

    fn record_quiesced(&mut self, proof: &DmaQuiesced) -> Result<(), BlkError> {
        self.validate_quiescence(proof)?;
        self.live_owner_mut()?.quiesced_epoch = Some(proof.epoch());
        Ok(())
    }

    fn close_after_quiesce(&mut self, epoch: ControllerEpoch) -> Result<(), BlkError> {
        let storage = self.take_after_quiesce(epoch)?;
        drop(storage);
        Ok(())
    }

    fn take_after_quiesce(&mut self, epoch: ControllerEpoch) -> Result<T, BlkError> {
        if self.live_owner()?.quiesced_epoch != Some(epoch) {
            return Err(BlkError::InvalidDmaProof);
        }
        let mut previous = ManuallyDrop::new(mem::replace(self, Self::Closed));
        let Self::Live(live) = &mut *previous else {
            let previous = unsafe {
                // SAFETY: the complete state remains inside `previous`; this
                // branch restores it rather than discarding ownership.
                ManuallyDrop::take(&mut previous)
            };
            *self = previous;
            return Err(BlkError::InvalidDmaProof);
        };
        Ok(unsafe {
            // SAFETY: matching quiescence was checked and `self` is Closed, so
            // this is the unique transition back to ordinary Rust ownership.
            ManuallyDrop::take(&mut live.storage)
        })
    }

    fn live_owner(&self) -> Result<&LiveQueueDmaOwner<T>, BlkError> {
        match self {
            Self::Live(live) => Ok(live),
            Self::Closed => Err(BlkError::Offline),
            Self::Quarantined(_) => Err(BlkError::Quarantined),
        }
    }

    fn live_owner_mut(&mut self) -> Result<&mut LiveQueueDmaOwner<T>, BlkError> {
        match self {
            Self::Live(live) => Ok(live),
            Self::Closed => Err(BlkError::Offline),
            Self::Quarantined(_) => Err(BlkError::Quarantined),
        }
    }

    #[cfg(test)]
    const fn is_live(&self) -> bool {
        matches!(self, Self::Live(_))
    }
}

impl<T> Drop for QueueDmaOwner<T> {
    fn drop(&mut self) {
        let Self::Live(live) = self else {
            return;
        };
        let quarantine = QuarantinedQueueDmaOwner {
            storage: unsafe {
                // SAFETY: the live variant is overwritten immediately after
                // moving its unique storage owner into quarantine.
                ManuallyDrop::new(ManuallyDrop::take(&mut live.storage))
            },
            controller_cookie: live.controller_cookie,
            active_epoch: live.active_epoch,
            reason: QueueDmaQuarantineReason::DroppedWithoutQuiescence,
        };
        unsafe {
            // SAFETY: overwrite prevents drop glue from observing the moved
            // live storage; quarantine suppresses ring destruction.
            ptr::write(self, Self::Quarantined(quarantine));
        }
    }
}

impl<T> fmt::Debug for QuarantinedQueueDmaOwner<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("QuarantinedQueueDmaOwner")
            .field("controller_cookie", &self.controller_cookie)
            .field("active_epoch", &self.active_epoch)
            .field("reason", &self.reason)
            .field("storage", &core::ptr::from_ref(&*self.storage))
            .finish()
    }
}

fn map_reserved_credit_error(error: virtio_drivers::Error) -> BlkError {
    match error {
        // The activation plan advertises exactly one pre-reserved hardware
        // credit. Once the runtime grants it, queue backpressure is a driver
        // contract violation rather than a condition the runtime may retry.
        virtio_drivers::Error::QueueFull | virtio_drivers::Error::NotReady => BlkError::Io,
        error => map_virtio_err_to_blk_err(error),
    }
}

#[cfg(test)]
mod tests {
    use alloc::sync::Arc;
    use core::sync::atomic::{AtomicUsize, Ordering};

    use rdif_block::{BlkError, ControllerEpoch, DmaQuiesced};
    use virtio_drivers::Error;

    use super::{QueueDmaOwner, map_reserved_credit_error};

    struct DropProbe(Arc<AtomicUsize>);

    impl Drop for DropProbe {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }

    #[test]
    fn reserved_credit_failure_is_not_reported_as_retryable_backpressure() {
        assert_eq!(map_reserved_credit_error(Error::QueueFull), BlkError::Io);
        assert_eq!(map_reserved_credit_error(Error::NotReady), BlkError::Io);
    }

    #[test]
    fn dropping_live_queue_without_quiescence_does_not_release_ring() {
        let drops = Arc::new(AtomicUsize::new(0));
        let owner = QueueDmaOwner::new(
            DropProbe(Arc::clone(&drops)),
            0x51a7,
            ControllerEpoch::INITIAL,
        );

        drop(owner);

        assert_eq!(drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn failed_close_keeps_the_live_ring_owner() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut owner = QueueDmaOwner::new(
            DropProbe(Arc::clone(&drops)),
            0x51a7,
            ControllerEpoch::INITIAL,
        );

        assert_eq!(
            owner.close_after_quiesce(ControllerEpoch::new(2)),
            Err(BlkError::InvalidDmaProof)
        );
        assert!(owner.is_live());
        drop(owner);
        assert_eq!(drops.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn matching_dma_proof_is_required_before_releasing_ring() {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut owner = QueueDmaOwner::new(
            DropProbe(Arc::clone(&drops)),
            0x51a7,
            ControllerEpoch::INITIAL,
        );
        let stale = unsafe {
            // SAFETY: the drop probe is not hardware-visible DMA.
            DmaQuiesced::new(ControllerEpoch::INITIAL, 0x51a7)
        };
        assert_eq!(
            owner.record_quiesced(&stale),
            Err(BlkError::InvalidDmaProof)
        );

        let matching = unsafe {
            // SAFETY: this value-only proof tests exact epoch ownership.
            DmaQuiesced::new(ControllerEpoch::new(2), 0x51a7)
        };
        owner.record_quiesced(&matching).unwrap();
        owner.close_after_quiesce(ControllerEpoch::new(2)).unwrap();

        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}
