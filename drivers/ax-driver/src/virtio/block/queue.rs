//! VirtIO block request DMA, descriptor, and quiescence invariants.

#[cfg(test)]
mod legacy;
mod owned;

use alloc::boxed::Box;
#[cfg(test)]
use alloc::sync::Arc;
use core::mem::ManuallyDrop;
#[cfg(test)]
use core::sync::atomic::{AtomicUsize, Ordering};

use dma_api::{DmaDirection, InFlightDma, PreparedDma};
#[cfg(test)]
pub(super) use legacy::{BlockQueue, virtio_queue_ids};
pub(super) use owned::VirtioOwnedQueue;
#[cfg(test)]
use rdif_block::CompletionSink;
use rdif_block::{
    BlkError, CompletedRequest, IdList, OwnedRequest, QueueExecution, QueueInfo, QueueKind,
    RequestId, RequestOp, SubmitError,
};
use virtio_drivers::{Error as VirtIoError, device::blk::SECTOR_SIZE, queue::VirtQueue};

use super::{VIRTIO_BLK_IRQ_SOURCE_ID, VIRTIO_BLK_QUEUE_ID, device::VirtIoBlkInner};
use crate::virtio::VirtIoTransport;

// Keep one IRQ-driven completion large enough to amortize the interrupt/drain
// wake cost while preserving the intentionally conservative max_inflight=1.
pub(super) const VIRTIO_BLK_DMA_BUFFER_SIZE: usize = 4 * 1024 * 1024;
pub(super) const VIRTIO_BLK_QUEUE_SIZE: usize = 16;

pub(super) struct ReclaimProofTracker {
    controller_cookie: usize,
    last_epoch: Option<u64>,
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
        // The CPU-pinned maintenance owner is the only caller of queue
        // methods. Tagged describes retained RequestId ownership; it does not
        // authorize a submitting task to touch the transport directly.
        execution: QueueExecution::Tagged,
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

unsafe fn add_read_descriptor(
    queue: &mut VirtQueue<crate::virtio::VirtIoHalImpl, VIRTIO_BLK_QUEUE_SIZE>,
    request: &[u8; 16],
    data: &mut [u8],
    response: &mut [u8; 1],
) -> Result<u16, VirtIoError> {
    unsafe {
        // SAFETY: the caller retains all three buffers in stable in-flight
        // storage until matching IRQ evidence consumes the descriptor.
        queue.add(&[request], &mut [data, response])
    }
}

unsafe fn add_write_descriptor(
    queue: &mut VirtQueue<crate::virtio::VirtIoHalImpl, VIRTIO_BLK_QUEUE_SIZE>,
    request: &[u8; 16],
    data: &[u8],
    response: &mut [u8; 1],
) -> Result<u16, VirtIoError> {
    unsafe {
        // SAFETY: the caller retains all three buffers in stable in-flight
        // storage until matching IRQ evidence consumes the descriptor.
        queue.add(&[request, data], &mut [response])
    }
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum VirtioDmaQuarantineReason {
    ResetAcknowledgementTimedOut,
    DroppedWithoutQuiescence,
}

/// Named owner for allocations that may still be reachable by the device.
///
/// These fields deliberately suppress ordinary Rust destruction because no
/// hardware stop proof exists. The retained controller owns this value and
/// exposes its reason and retained-resource facts to diagnostics. A normal
/// acknowledged-reset path never constructs this type: it drops the old
/// virtqueue and reclaims request DMA only while consuming `DmaQuiesced`.
pub(super) struct VirtioDmaQuarantine {
    reason: VirtioDmaQuarantineReason,
    queue: Option<ManuallyDrop<VirtQueue<crate::virtio::VirtIoHalImpl, VIRTIO_BLK_QUEUE_SIZE>>>,
    inflight: Option<ManuallyDrop<InflightRequest>>,
    descriptor_storage: Option<ManuallyDrop<Box<InflightStorage>>>,
}

impl VirtioDmaQuarantine {
    pub(super) fn retain(
        reason: VirtioDmaQuarantineReason,
        queue: Option<VirtQueue<crate::virtio::VirtIoHalImpl, VIRTIO_BLK_QUEUE_SIZE>>,
        inflight: Option<InflightRequest>,
        descriptor_storage: Option<Box<InflightStorage>>,
    ) -> Option<Self> {
        let quarantine = Self {
            reason,
            queue: queue.map(ManuallyDrop::new),
            inflight: inflight.map(ManuallyDrop::new),
            descriptor_storage: descriptor_storage.map(ManuallyDrop::new),
        };
        quarantine.blocks_reinitialization().then_some(quarantine)
    }

    pub(super) const fn reason(&self) -> VirtioDmaQuarantineReason {
        self.reason
    }

    pub(super) fn retains_queue(&self) -> bool {
        self.queue.is_some()
    }

    pub(super) fn retains_request(&self) -> bool {
        self.inflight.is_some()
    }

    pub(super) fn retains_descriptor_storage(&self) -> bool {
        self.descriptor_storage.is_some()
    }

    pub(super) fn blocks_reinitialization(&self) -> bool {
        // Reading the reason here makes the containment cause part of the
        // auditable state rather than decoration on an anonymous retention.
        match self.reason() {
            VirtioDmaQuarantineReason::ResetAcknowledgementTimedOut
            | VirtioDmaQuarantineReason::DroppedWithoutQuiescence => {
                self.retains_queue() || self.retains_request() || self.retains_descriptor_storage()
            }
        }
    }

    #[cfg(test)]
    fn release_after_quiesce(
        mut self,
        _proof: &rdif_block::DmaQuiesced,
        sink: &mut dyn CompletionSink,
    ) {
        let queue = self.queue.take().map(ManuallyDrop::into_inner);
        let inflight = self.inflight.take().map(ManuallyDrop::into_inner);
        let descriptor_storage = self.descriptor_storage.take().map(ManuallyDrop::into_inner);

        // The proof establishes that no descriptor or ring allocation remains
        // reachable by the device. Restore ordinary Rust ownership before
        // releasing memory or returning a request to the runtime.
        drop(queue);
        if let Some(mut inflight) = inflight {
            // SAFETY: the controller-bound proof was validated by the queue's
            // monotonic `ReclaimProofTracker` before this quarantine was moved.
            let completed = unsafe { inflight.dma.complete_after_quiesce() };
            inflight.request.data = Some(completed.into_cpu_buffer());
            sink.complete(CompletedRequest::new(
                inflight.id,
                Err(BlkError::Cancelled),
                inflight.request,
            ));
        }
        drop(descriptor_storage);
    }
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
            self.quarantine_unproven_dma(VirtioDmaQuarantineReason::DroppedWithoutQuiescence);
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
