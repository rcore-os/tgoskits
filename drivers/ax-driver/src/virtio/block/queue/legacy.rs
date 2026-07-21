//! v0.12 queue surface retained only by migration regression tests.

use alloc::sync::Arc;

use rdif_block::{
    BlkError, CompletedRequest, CompletionSink, IdList, OwnedRequest, QueueEventBatch, QueueInfo,
    RequestId, ServiceProgress, ServiceRerunReason, SubmitError, SubmitOutcome,
};
use virtio_drivers::Error as VirtIoError;

use super::{
    InflightRequest, ReclaimProofTracker, VIRTIO_BLK_QUEUE_SIZE, complete_consumed_inflight,
    map_virtio_completion_err_to_blk_err, map_virtio_err_to_blk_err, pop_used_descriptor,
    prepare_virtio_dma, take_inflight_after_used_descriptor,
};
use crate::virtio::{
    VirtIoTransport,
    block::{
        VIRTIO_BLK_QUEUE_ID,
        device::{VirtIoBlkDevice, VirtIoBlkInner},
        irq::VirtioRegisterMappingLease,
    },
};

const VIRTIO_BLK_SERVICE_BUDGET: usize = 64;

pub(in crate::virtio::block) struct BlockQueue<T: VirtIoTransport> {
    id: usize,
    raw: Arc<VirtIoBlkDevice<T>>,
    _register_mapping: VirtioRegisterMappingLease,
    reclaim_proof: ReclaimProofTracker,
}

impl<T: VirtIoTransport> BlockQueue<T> {
    pub(in crate::virtio::block) fn new(
        raw: Arc<VirtIoBlkDevice<T>>,
        register_mapping: VirtioRegisterMappingLease,
    ) -> Self {
        let controller_cookie = core::ptr::from_ref(raw.as_ref()).expose_provenance();
        Self {
            id: VIRTIO_BLK_QUEUE_ID,
            raw,
            _register_mapping: register_mapping,
            reclaim_proof: ReclaimProofTracker {
                controller_cookie,
                last_epoch: None,
            },
        }
    }

    pub(in crate::virtio::block) fn for_test(raw: Arc<VirtIoBlkDevice<T>>) -> Self {
        Self::new(
            raw,
            crate::virtio::block::irq::test_register_mapping_lease(),
        )
    }
}

pub(in crate::virtio::block) fn virtio_queue_ids() -> IdList {
    let mut queues = IdList::none();
    queues.insert(VIRTIO_BLK_QUEUE_ID);
    queues
}

impl<T: VirtIoTransport> rdif_block::IQueue for BlockQueue<T> {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        let blocks = self.raw.capacity_if_ready().unwrap_or(0);
        let mut info = super::virtio_queue_info(blocks);
        info.device.read_only = self.raw.read_only_if_ready().unwrap_or(false);
        info
    }

    fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        if let Err(error) = rdif_block::validate_owned_request(self.info(), &request) {
            return Err(SubmitError::new(id, error, request));
        }
        self.raw.with_task(|inner| inner.submit_owned(id, request))
    }

    fn service_events(
        &mut self,
        events: &QueueEventBatch<'_>,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        if events.queue_id() != self.id {
            return Err(BlkError::InvalidRequest);
        }
        self.raw.with_task(|inner| inner.service_used(events, sink))
    }

    fn reclaim_after_quiesce(
        &mut self,
        proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        self.reclaim_proof.validate(proof)?;
        self.raw
            .with_task(|inner| inner.reclaim_after_quiesce(proof, sink));
        self.reclaim_proof.commit(proof);
        Ok(())
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        self.raw.with_task(VirtIoBlkInner::shutdown)
    }
}

impl<T: VirtIoTransport> VirtIoBlkInner<T> {
    fn submit_owned(
        &mut self,
        id: RequestId,
        request: OwnedRequest,
    ) -> Result<SubmitOutcome, SubmitError> {
        if self.inflight.is_some() {
            return Err(SubmitError::new(id, BlkError::Retry, request));
        }

        let (op, mut request, prepared) = prepare_virtio_dma(id, request)?;
        let Some(storage) = self.descriptor_storage.as_deref_mut() else {
            request.data = Some(prepared.into_cpu_buffer());
            return Err(SubmitError::new(id, BlkError::Offline, request));
        };
        storage.prepare(op, request.lba);
        let ptr = prepared.cpu_ptr();
        let len = prepared.len().get();
        let Some(queue) = self.queue.as_mut() else {
            request.data = Some(prepared.into_cpu_buffer());
            return Err(SubmitError::new(id, BlkError::Retry, request));
        };
        let token = match op {
            super::InflightOp::Read => {
                // SAFETY: `prepared` retains this stable allocation until the
                // matching descriptor is consumed.
                let buffer = unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), len) };
                unsafe {
                    submit_read(
                        &mut self.transport,
                        queue,
                        &storage.req,
                        buffer,
                        &mut storage.resp,
                    )
                }
            }
            super::InflightOp::Write => {
                // SAFETY: the allocation remains device-owned until used-ring
                // consumption or a DMA-quiesced recovery.
                let buffer = unsafe { core::slice::from_raw_parts(ptr.as_ptr().cast_const(), len) };
                unsafe {
                    submit_write(
                        &mut self.transport,
                        queue,
                        &storage.req,
                        buffer,
                        &mut storage.resp,
                    )
                }
            }
        };
        let token = match token {
            Ok(token) => token,
            Err(error) => {
                request.data = Some(prepared.into_cpu_buffer());
                return Err(SubmitError::new(
                    id,
                    map_virtio_err_to_blk_err(error),
                    request,
                ));
            }
        };
        // SAFETY: the accepted descriptor and exact backing remain retained.
        let dma = unsafe { prepared.into_in_flight() };
        self.inflight = Some(InflightRequest {
            id,
            token,
            op,
            request,
            dma,
        });
        Ok(SubmitOutcome::Queued)
    }

    fn service_used(
        &mut self,
        events: &QueueEventBatch<'_>,
        sink: &mut dyn CompletionSink,
    ) -> Result<ServiceProgress, BlkError> {
        for _ in 0..VIRTIO_BLK_SERVICE_BUDGET {
            let Some(inflight) = self.inflight.as_ref() else {
                return Ok(ServiceProgress::Idle);
            };
            let Some(used_token) = self
                .queue
                .as_ref()
                .and_then(virtio_drivers::queue::VirtQueue::peek_used)
            else {
                return Ok(ServiceProgress::Idle);
            };
            if used_token != inflight.token {
                return Err(BlkError::Io);
            }
            let queue = self.queue.as_mut().ok_or(BlkError::Offline)?;
            let storage = self
                .descriptor_storage
                .as_deref_mut()
                .ok_or(BlkError::Offline)?;
            let inflight = take_inflight_after_used_descriptor(&mut self.inflight, |inflight| {
                pop_used_descriptor(queue, storage, inflight)
            })?;
            let result = super::virtio_response_result(storage.resp[0])
                .map_err(map_virtio_completion_err_to_blk_err);
            sink.complete(complete_consumed_inflight(inflight, result));
        }
        Ok(events.requeue_service(ServiceRerunReason::CompletionBudget))
    }

    fn reclaim_after_quiesce(
        &mut self,
        proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) {
        if let Some(quarantine) = self.dma_quarantine.take() {
            quarantine.release_after_quiesce(proof, sink);
            return;
        }
        let Some(mut inflight) = self.inflight.take() else {
            return;
        };
        // SAFETY: the controller-bound proof establishes DMA ownership return.
        let completed = unsafe { inflight.dma.complete_after_quiesce() };
        inflight.request.data = Some(completed.into_cpu_buffer());
        sink.complete(CompletedRequest::new(
            inflight.id,
            Err(BlkError::Cancelled),
            inflight.request,
        ));
    }

    fn shutdown(&mut self) -> Result<(), BlkError> {
        if self.inflight.is_some() {
            Err(BlkError::Busy)
        } else {
            Ok(())
        }
    }
}

unsafe fn submit_read<T: VirtIoTransport>(
    transport: &mut T,
    queue: &mut virtio_drivers::queue::VirtQueue<
        crate::virtio::VirtIoHalImpl,
        VIRTIO_BLK_QUEUE_SIZE,
    >,
    request: &[u8; 16],
    data: &mut [u8],
    response: &mut [u8; 1],
) -> Result<u16, VirtIoError> {
    let token = unsafe { super::add_read_descriptor(queue, request, data, response)? };
    if queue.should_notify() {
        transport.notify(VIRTIO_BLK_QUEUE_ID as u16);
    }
    Ok(token)
}

unsafe fn submit_write<T: VirtIoTransport>(
    transport: &mut T,
    queue: &mut virtio_drivers::queue::VirtQueue<
        crate::virtio::VirtIoHalImpl,
        VIRTIO_BLK_QUEUE_SIZE,
    >,
    request: &[u8; 16],
    data: &[u8],
    response: &mut [u8; 1],
) -> Result<u16, VirtIoError> {
    let token = unsafe { super::add_write_descriptor(queue, request, data, response)? };
    if queue.should_notify() {
        transport.notify(VIRTIO_BLK_QUEUE_ID as u16);
    }
    Ok(token)
}
