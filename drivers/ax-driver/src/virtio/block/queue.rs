//! Owned-request RDIF queue and VirtIO descriptor lifecycle.

use alloc::sync::Arc;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

use dma_api::{DmaDirection, InFlightDma, PreparedDma};
use rdif_block::{
    BlkError, CompletedRequest, CompletionSink, DispatchMode, IdList, OwnedRequest,
    QueueEventBatch, QueueInfo, QueueKind, RequestId, RequestOp, ServiceContinuationReason,
    ServiceProgress, SubmitError, SubmitOutcome,
};
use virtio_drivers::{Error as VirtIoError, device::blk::SECTOR_SIZE};

use super::{
    VIRTIO_BLK_IRQ_SOURCE_ID, VIRTIO_BLK_QUEUE_ID,
    device::{VirtIoBlkDevice, VirtIoBlkInner},
};
use crate::virtio::VirtIoTransport;

// Keep one IRQ-driven completion large enough to amortize the interrupt/drain
// wake cost while preserving the intentionally conservative max_inflight=1.
pub(super) const VIRTIO_BLK_DMA_BUFFER_SIZE: usize = 4 * 1024 * 1024;
const VIRTIO_BLK_SERVICE_BUDGET: usize = 64;
pub(super) const VIRTIO_BLK_QUEUE_SIZE: usize = 16;

pub(super) struct BlockQueue<T: VirtIoTransport> {
    id: usize,
    // Keeps the transport and in-flight descriptor state alive. Platform PCI
    // MSI-X/INTx leases live outside this Arc and must remain owned by the OS
    // Interface holder until IRQ synchronization and queue shutdown complete.
    raw: Arc<VirtIoBlkDevice<T>>,
    reclaim_proof: ReclaimProofTracker,
}

pub(super) struct ReclaimProofTracker {
    controller_cookie: usize,
    last_epoch: Option<u64>,
}

impl<T: VirtIoTransport> BlockQueue<T> {
    pub(super) fn new(raw: Arc<VirtIoBlkDevice<T>>) -> Self {
        let controller_cookie = core::ptr::from_ref(raw.as_ref()).expose_provenance();
        Self {
            id: VIRTIO_BLK_QUEUE_ID,
            raw,
            reclaim_proof: ReclaimProofTracker {
                controller_cookie,
                last_epoch: None,
            },
        }
    }
}

pub(super) fn virtio_queue_ids() -> IdList {
    let mut queues = IdList::none();
    queues.insert(VIRTIO_BLK_QUEUE_ID);
    queues
}

pub(super) fn virtio_queue_info(blocks: u64) -> QueueInfo {
    let mut sources = IdList::none();
    sources.insert(VIRTIO_BLK_IRQ_SOURCE_ID);
    QueueInfo {
        id: VIRTIO_BLK_QUEUE_ID,
        device: rdif_block::DeviceInfo {
            name: Some("virtio-blk"),
            ..rdif_block::DeviceInfo::new(blocks, SECTOR_SIZE)
        },
        limits: rdif_block::QueueLimits {
            dma_domain: dma_api::DmaDomainId::legacy_global(),
            dma_mask: u64::MAX,
            dma_alignment: 0x1000,
            max_inflight: 1,
            max_blocks_per_request: (VIRTIO_BLK_DMA_BUFFER_SIZE / SECTOR_SIZE) as u32,
            max_segments: 1,
            max_segment_size: VIRTIO_BLK_DMA_BUFFER_SIZE,
            request_timeout_ns: rdif_block::DEFAULT_REQUEST_TIMEOUT_NS,
            supported_flags: rdif_block::RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        },
        kind: QueueKind::Interrupt { sources },
        // The hctx queue lock and the adapter's short task/IRQ gate serialize
        // transport state. Submit can therefore use the direct fast path; an
        // hctx lock conflict falls back to its software staging queue, while
        // IRQ acknowledgement contention remains a typed worker continuation.
        dispatch_mode: DispatchMode::Direct,
    }
}

impl<T: VirtIoTransport> rdif_block::IQueue for BlockQueue<T> {
    fn id(&self) -> usize {
        self.id
    }

    fn info(&self) -> QueueInfo {
        let blocks = self.raw.capacity_if_ready().unwrap_or(0);
        let mut info = virtio_queue_info(blocks);
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
        self.raw
            .try_with_task(|inner| inner.service_used(events, sink))
            .unwrap_or_else(|| {
                Ok(events.continue_service(ServiceContinuationReason::RetainedFacts))
            })
    }

    fn reclaim_after_quiesce(
        &mut self,
        proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) -> Result<(), BlkError> {
        self.reclaim_proof.validate(proof)?;
        self.raw
            .with_task(|inner| inner.reclaim_after_quiesce(sink));
        self.reclaim_proof.commit(proof);
        Ok(())
    }

    fn shutdown(&mut self, sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        self.raw.with_task(|inner| inner.shutdown(sink))
    }
}

impl ReclaimProofTracker {
    #[cfg(test)]
    pub(super) const fn for_test(controller_cookie: usize) -> Self {
        Self {
            controller_cookie,
            last_epoch: None,
        }
    }

    pub(super) fn validate(&self, proof: &rdif_block::DmaQuiesced) -> Result<(), BlkError> {
        if proof.controller_cookie() != self.controller_cookie
            || self
                .last_epoch
                .is_some_and(|last_epoch| proof.epoch().get() <= last_epoch)
        {
            return Err(BlkError::InvalidDmaProof);
        }
        Ok(())
    }

    pub(super) fn commit(&mut self, proof: &rdif_block::DmaQuiesced) {
        self.last_epoch = Some(proof.epoch().get());
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
            InflightOp::Read => {
                // SAFETY: `prepared` exclusively owns this stable allocation.
                // The exact pointer and length are retained in `InFlightDma`
                // until IRQ continuation consumes the matching descriptor.
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
            InflightOp::Write => {
                // SAFETY: The same prepared allocation remains exclusively
                // device-owned until the matching used descriptor is consumed.
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

        // SAFETY: the non-blocking VirtIO submission above accepted `token`
        // while the same task-side exclusion still prevents the IRQ path from
        // observing the descriptor. IRQ continuation or recovery returns
        // ownership only after the matching descriptor/device is quiesced.
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
            let Some(used_token) = self.peek_used() else {
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
            let result = virtio_response_result(storage.resp[0])
                .map_err(map_virtio_completion_err_to_blk_err);
            sink.complete(complete_consumed_inflight(inflight, result));
        }
        Ok(events.continue_service(ServiceContinuationReason::CompletionBudget))
    }

    fn peek_used(&self) -> Option<u16> {
        self.queue
            .as_ref()
            .and_then(virtio_drivers::queue::VirtQueue::peek_used)
    }

    fn reclaim_after_quiesce(&mut self, sink: &mut dyn CompletionSink) {
        let Some(mut inflight) = self.inflight.take() else {
            return;
        };
        // SAFETY: the caller can invoke this method only while holding the
        // controller-bound `DmaQuiesced` proof. Therefore the accepted
        // descriptor can no longer access this exact DMA buffer.
        let completed = unsafe { inflight.dma.complete_after_quiesce() };
        inflight.request.data = Some(completed.into_cpu_buffer());
        sink.complete(CompletedRequest::new(
            inflight.id,
            Err(BlkError::Cancelled),
            inflight.request,
        ));
    }

    fn shutdown(&mut self, _sink: &mut dyn CompletionSink) -> Result<(), BlkError> {
        if self.inflight.is_some() {
            return Err(BlkError::Busy);
        }
        Ok(())
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
    let token = unsafe {
        // SAFETY: the caller retains all three buffers in stable in-flight
        // storage until IRQ continuation consumes this exact descriptor.
        queue.add(&[request], &mut [data, response])?
    };
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
    let token = unsafe {
        // SAFETY: the caller retains all three buffers in stable in-flight
        // storage until IRQ continuation consumes this exact descriptor.
        queue.add(&[request, data], &mut [response])?
    };
    if queue.should_notify() {
        transport.notify(VIRTIO_BLK_QUEUE_ID as u16);
    }
    Ok(token)
}

pub(super) fn take_inflight_after_used_descriptor(
    slot: &mut Option<InflightRequest>,
    pop_descriptor: impl FnOnce(&mut InflightRequest) -> Result<(), VirtIoError>,
) -> Result<InflightRequest, BlkError> {
    let mut inflight = slot.take().ok_or(BlkError::Io)?;
    if let Err(error) = pop_descriptor(&mut inflight) {
        // A failed `pop_used` does not prove that the device relinquished the
        // descriptor. Keep every buffer and the runtime RequestId in the live
        // slot so controller recovery can return them only after DmaQuiesced.
        *slot = Some(inflight);
        return Err(map_virtio_completion_err_to_blk_err(error));
    }
    Ok(inflight)
}

fn pop_used_descriptor(
    queue: &mut virtio_drivers::queue::VirtQueue<
        crate::virtio::VirtIoHalImpl,
        VIRTIO_BLK_QUEUE_SIZE,
    >,
    storage: &mut InflightStorage,
    inflight: &mut InflightRequest,
) -> Result<(), VirtIoError> {
    let ptr = inflight.dma.cpu_ptr();
    let len = inflight.dma.len().get();
    match inflight.op {
        InflightOp::Read => {
            // SAFETY: the retained in-flight allocation and descriptor
            // storage are the exact buffers submitted under this token.
            let buffer = unsafe { core::slice::from_raw_parts_mut(ptr.as_ptr(), len) };
            unsafe {
                queue
                    .pop_used(
                        inflight.token,
                        &[&storage.req],
                        &mut [buffer, &mut storage.resp],
                    )
                    .map(|_| ())
            }
        }
        InflightOp::Write => {
            // SAFETY: the retained in-flight allocation and descriptor
            // storage are the exact buffers submitted under this token.
            let buffer = unsafe { core::slice::from_raw_parts(ptr.as_ptr().cast_const(), len) };
            unsafe {
                queue
                    .pop_used(
                        inflight.token,
                        &[&storage.req, buffer],
                        &mut [&mut storage.resp],
                    )
                    .map(|_| ())
            }
        }
    }
}

fn complete_consumed_inflight(
    mut inflight: InflightRequest,
    result: Result<(), BlkError>,
) -> CompletedRequest {
    // SAFETY: `pop_used_descriptor` consumed the matching descriptor before
    // this function receives the request, so the device relinquished DMA.
    let completed = unsafe { inflight.dma.complete_after_quiesce() };
    inflight.request.data = Some(completed.into_cpu_buffer());
    CompletedRequest::new(inflight.id, result, inflight.request)
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct DmaDropFacts {
    pub(super) failure_reset_in_progress: bool,
    pub(super) queue_configured: bool,
    pub(super) request_inflight: bool,
    pub(super) reset_acknowledged: bool,
}

impl DmaDropFacts {
    pub(super) const fn requires_quarantine(self) -> bool {
        self.failure_reset_in_progress
            || self.request_inflight
            || (self.queue_configured && !self.reset_acknowledged)
    }
}

impl<T: VirtIoTransport> Drop for VirtIoBlkInner<T> {
    fn drop(&mut self) {
        let queue_configured = self.queue.is_some();
        let drop_facts = DmaDropFacts {
            failure_reset_in_progress: self.init_phase
                == super::initialization::VirtioBlockInitPhase::FailureReset,
            queue_configured,
            request_inflight: self.inflight.is_some(),
            // `queue_unset` is not a portable stop proof: the PCI transport
            // implements it as a no-op. Device status zero is the only
            // transport-wide acknowledgement available through this trait.
            reset_acknowledged: !queue_configured || self.transport.get_status().is_empty(),
        };
        if drop_facts.requires_quarantine() {
            // A safe Drop cannot assume that the runtime honored shutdown and
            // produced DmaQuiesced. Retain the virtqueue plus every descriptor
            // backing so a late device access cannot target freed Rust memory.
            self.quarantine_unproven_dma();
            return;
        }
        if self.queue.is_some() {
            self.transport.queue_unset(VIRTIO_BLK_QUEUE_ID as u16);
        }
    }
}

pub(super) fn prepare_virtio_dma(
    id: RequestId,
    mut request: OwnedRequest,
) -> Result<(InflightOp, OwnedRequest, PreparedDma), SubmitError> {
    let (op, direction) = match request.op {
        RequestOp::Read => (InflightOp::Read, DmaDirection::FromDevice),
        RequestOp::Write => (InflightOp::Write, DmaDirection::ToDevice),
        RequestOp::Flush | RequestOp::Discard | RequestOp::WriteZeroes => {
            return Err(SubmitError::new(id, BlkError::NotSupported, request));
        }
    };
    let Some(data) = request.data.take() else {
        return Err(SubmitError::new(id, BlkError::InvalidRequest, request));
    };
    if !dma_direction_supports(data.direction(), direction)
        || data.domain_id() != dma_api::DmaDomainId::legacy_global()
    {
        request.data = Some(data);
        return Err(SubmitError::new(id, BlkError::InvalidRequest, request));
    }
    Ok((op, request, data.prepare_for_device()))
}

fn dma_direction_supports(actual: DmaDirection, expected: DmaDirection) -> bool {
    actual == expected || actual == DmaDirection::Bidirectional
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum InflightOp {
    Read,
    Write,
}

pub(super) struct InflightRequest {
    id: RequestId,
    token: u16,
    op: InflightOp,
    request: OwnedRequest,
    dma: InFlightDma,
}

pub(super) struct InflightStorage {
    pub(super) req: [u8; 16],
    pub(super) resp: [u8; 1],
    #[cfg(test)]
    drop_counter: Option<Arc<AtomicUsize>>,
}

impl Default for InflightStorage {
    fn default() -> Self {
        Self {
            req: [0; 16],
            // VIRTIO_BLK_S_UNSUPP/IOERR/OK are terminal; 3 keeps unconsumed
            // storage observably non-terminal until the device writes it.
            resp: [3],
            #[cfg(test)]
            drop_counter: None,
        }
    }
}

impl InflightStorage {
    pub(super) fn prepare(&mut self, op: InflightOp, sector: u64) {
        let request_type = match op {
            InflightOp::Read => 0_u32,
            InflightOp::Write => 1_u32,
        };
        self.req = [0; 16];
        self.req[..4].copy_from_slice(&request_type.to_le_bytes());
        self.req[8..].copy_from_slice(&sector.to_le_bytes());
        self.resp = [3];
    }

    #[cfg(test)]
    pub(super) fn req_addr(&self) -> usize {
        core::ptr::addr_of!(self.req) as usize
    }

    #[cfg(test)]
    pub(super) fn resp_addr(&self) -> usize {
        core::ptr::addr_of!(self.resp) as usize
    }

    #[cfg(test)]
    pub(super) fn with_drop_counter(drop_counter: Arc<AtomicUsize>) -> Self {
        Self {
            drop_counter: Some(drop_counter),
            ..Self::default()
        }
    }
}

#[cfg(test)]
impl Drop for InflightStorage {
    fn drop(&mut self) {
        if let Some(drop_counter) = &self.drop_counter {
            drop_counter.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
impl InflightRequest {
    pub(super) fn for_test(
        id: RequestId,
        token: u16,
        op: InflightOp,
        request: OwnedRequest,
        dma: InFlightDma,
    ) -> Self {
        Self {
            id,
            token,
            op,
            request,
            dma,
        }
    }
}

fn virtio_response_result(status: u8) -> Result<(), VirtIoError> {
    match status {
        0 => Ok(()),
        1 => Err(VirtIoError::IoError),
        2 => Err(VirtIoError::Unsupported),
        3 => Err(VirtIoError::NotReady),
        _ => Err(VirtIoError::IoError),
    }
}

fn map_virtio_err_to_blk_err(err: VirtIoError) -> rdif_block::BlkError {
    match err {
        VirtIoError::QueueFull | VirtIoError::NotReady => rdif_block::BlkError::Retry,
        VirtIoError::WrongToken
        | VirtIoError::ConfigSpaceTooSmall
        | VirtIoError::ConfigSpaceMissing => rdif_block::BlkError::Other("bad internal state"),
        VirtIoError::AlreadyUsed => rdif_block::BlkError::Other("already exists"),
        VirtIoError::InvalidParam => rdif_block::BlkError::InvalidRequest,
        VirtIoError::DmaError => rdif_block::BlkError::NoMemory,
        VirtIoError::IoError => rdif_block::BlkError::Io,
        VirtIoError::Unsupported => rdif_block::BlkError::NotSupported,
        VirtIoError::SocketDeviceError(_) => rdif_block::BlkError::Other("socket error"),
    }
}

fn map_virtio_completion_err_to_blk_err(err: VirtIoError) -> rdif_block::BlkError {
    match map_virtio_err_to_blk_err(err) {
        rdif_block::BlkError::Retry => rdif_block::BlkError::Io,
        err => err,
    }
}
