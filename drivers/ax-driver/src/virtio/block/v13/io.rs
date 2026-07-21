//! Queue state retained by the combined VirtIO control/I/O owner.

use alloc::boxed::Box;

use rdif_block::{
    AcceptedRequest, BlkError, CompletionSink, ControllerEpoch, DriverDeviceKey, UnacceptedRequest,
};

use super::super::{
    VIRTIO_BLK_QUEUE_ID,
    notify::VirtioQueueNotifyPort,
    queue::{InflightStorage, VirtioOwnedQueue},
};

pub(super) struct VirtioV13IoDomain {
    domain: rdif_block::OwnershipDomainId,
    device_key: DriverDeviceKey,
    queue: Option<VirtioOwnedQueue>,
    active_epoch: ControllerEpoch,
    reclaimed_epoch: Option<ControllerEpoch>,
    rebuilding_epoch: Option<ControllerEpoch>,
    resumed_epoch: Option<ControllerEpoch>,
    online: bool,
}

impl VirtioV13IoDomain {
    pub(super) fn new(
        domain: rdif_block::OwnershipDomainId,
        device_key: DriverDeviceKey,
        queue: VirtioOwnedQueue,
    ) -> Self {
        Self {
            domain,
            device_key,
            queue: Some(queue),
            active_epoch: ControllerEpoch::INITIAL,
            reclaimed_epoch: None,
            rebuilding_epoch: None,
            resumed_epoch: None,
            online: true,
        }
    }

    pub(super) const fn domain_id(&self) -> rdif_block::OwnershipDomainId {
        self.domain
    }

    pub(super) const fn queue_count(&self) -> usize {
        1
    }

    pub(super) const fn active_epoch(&self) -> ControllerEpoch {
        self.active_epoch
    }

    pub(super) fn reclaimed_epoch_matches(&self, epoch: ControllerEpoch) -> bool {
        !self.online && self.reclaimed_epoch == Some(epoch)
    }

    pub(super) fn set_interrupts(&mut self, enabled: bool) {
        if let Some(queue) = self.queue.as_mut() {
            queue.set_interrupts(enabled);
        }
    }

    pub(super) fn submit_owned(
        &mut self,
        queue_id: usize,
        logical_device: DriverDeviceKey,
        id: rdif_block::RequestId,
        request: rdif_block::OwnedRequest,
    ) -> Result<AcceptedRequest, UnacceptedRequest> {
        if !self.online || queue_id != VIRTIO_BLK_QUEUE_ID || logical_device != self.device_key {
            return Err(UnacceptedRequest::new(
                id,
                if self.online {
                    BlkError::InvalidRequest
                } else {
                    BlkError::Offline
                },
                request,
            ));
        }
        let Some(queue) = self.queue.as_mut() else {
            return Err(UnacceptedRequest::new(id, BlkError::Offline, request));
        };
        queue.submit_owned(logical_device, id, request)
    }

    pub(super) fn service_queue_fact(
        &mut self,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        if !self.online {
            return Err(BlkError::Offline);
        }
        self.queue
            .as_mut()
            .ok_or(BlkError::Offline)?
            .service_queue_fact(sink)
    }

    pub(super) fn reclaim_after_quiesce(
        &mut self,
        proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        if proof.epoch() <= self.active_epoch
            || self
                .reclaimed_epoch
                .is_some_and(|epoch| proof.epoch() <= epoch)
        {
            return Err(BlkError::InvalidDmaProof);
        }
        self.queue
            .as_mut()
            .ok_or(BlkError::Offline)?
            .reclaim_after_quiesce(proof, sink)?;
        self.online = false;
        self.reclaimed_epoch = Some(proof.epoch());
        self.rebuilding_epoch = None;
        self.resumed_epoch = None;
        Ok(())
    }

    pub(super) fn begin_rebuild(
        &mut self,
        epoch: ControllerEpoch,
    ) -> Result<(VirtioQueueNotifyPort, Box<InflightStorage>), BlkError> {
        if self.online
            || self.reclaimed_epoch != Some(epoch)
            || self.rebuilding_epoch.is_some()
            || self.resumed_epoch.is_some()
        {
            return Err(BlkError::InvalidDmaProof);
        }
        let queue = self.queue.take().ok_or(BlkError::Offline)?;
        match queue.into_reinitialize_parts(epoch) {
            Ok(parts) => {
                self.rebuilding_epoch = Some(epoch);
                Ok(parts)
            }
            Err(queue) => {
                self.queue = Some(*queue);
                Err(BlkError::InvalidDmaProof)
            }
        }
    }

    pub(super) fn install_rebuilt_queue(
        &mut self,
        epoch: ControllerEpoch,
        queue: VirtioOwnedQueue,
    ) -> Result<(), Box<VirtioOwnedQueue>> {
        if self.queue.is_some()
            || self.reclaimed_epoch != Some(epoch)
            || self.rebuilding_epoch != Some(epoch)
        {
            return Err(Box::new(queue));
        }
        self.queue = Some(queue);
        self.active_epoch = epoch;
        Ok(())
    }

    pub(super) fn resume_after_reinitialize(
        &mut self,
        epoch: ControllerEpoch,
    ) -> Result<(), BlkError> {
        if self.online
            || self.queue.is_none()
            || self.active_epoch != epoch
            || self.reclaimed_epoch != Some(epoch)
            || self.rebuilding_epoch != Some(epoch)
            || self.resumed_epoch == Some(epoch)
        {
            return Err(BlkError::InvalidDmaProof);
        }
        self.online = true;
        self.resumed_epoch = Some(epoch);
        Ok(())
    }

    pub(super) fn shutdown(&mut self) -> Result<(), BlkError> {
        let epoch = shutdown_epoch(self.online, self.reclaimed_epoch)?;
        self.queue
            .as_mut()
            .ok_or(BlkError::Offline)?
            .shutdown(epoch)
    }
}

fn shutdown_epoch(
    online: bool,
    reclaimed_epoch: Option<ControllerEpoch>,
) -> Result<ControllerEpoch, BlkError> {
    if online {
        return Err(BlkError::Busy);
    }
    reclaimed_epoch.ok_or(BlkError::InvalidDmaProof)
}

#[cfg(test)]
mod tests {
    use rdif_block::ControllerEpoch;

    use super::shutdown_epoch;

    #[test]
    fn live_or_unreclaimed_domain_cannot_release_queue_dma() {
        assert!(shutdown_epoch(true, Some(ControllerEpoch::new(2))).is_err());
        assert!(shutdown_epoch(false, None).is_err());
        assert_eq!(
            shutdown_epoch(false, Some(ControllerEpoch::new(2))),
            Ok(ControllerEpoch::new(2))
        );
    }
}
