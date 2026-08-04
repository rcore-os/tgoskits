use core::num::NonZeroUsize;

use dma_api::{CompletedDma, CpuDmaBuffer, DeviceDma, DmaConstraints, DmaDirection, PreparedDma};
use rdif_block::{BlkError, QueueLimits};

use crate::os::dma_op;

pub(super) fn prepare_read(limits: QueueLimits, len: usize) -> Result<PreparedDma, BlkError> {
    allocate(limits, len, DmaDirection::FromDevice).map(CpuDmaBuffer::prepare_for_device)
}

#[cfg(any(feature = "ext4", feature = "fat"))]
pub(super) fn prepare_write(limits: QueueLimits, source: &[u8]) -> Result<PreparedDma, BlkError> {
    let mut buffer = allocate(limits, source.len(), DmaDirection::ToDevice)?;
    buffer.copy_to_device_from_slice(source);
    Ok(buffer.prepare_for_device())
}

pub(super) fn complete_without_submit(data: Option<PreparedDma>) -> Option<CompletedDma> {
    data.map(PreparedDma::complete_without_device)
}

fn allocate(
    limits: QueueLimits,
    len: usize,
    direction: DmaDirection,
) -> Result<CpuDmaBuffer, BlkError> {
    if limits.dma_alignment == 0
        || limits.dma_length_alignment == 0
        || !len.is_multiple_of(limits.dma_length_alignment)
    {
        return Err(BlkError::InvalidRequest);
    }
    let dma_op = dma_op().ok_or(BlkError::Io)?;
    let mut constraints = DmaConstraints::new(limits.dma_mask).with_align(limits.dma_alignment);
    if let Some(boundary) = limits.segment_boundary {
        if !boundary.is_power_of_two() {
            return Err(BlkError::InvalidRequest);
        }
        constraints = constraints.with_boundary(boundary);
    }
    constraints = constraints.with_max_segment_size(limits.max_segment_size);
    let device =
        DeviceDma::new(limits.dma_domain, limits.dma_mask, dma_op).with_constraints(constraints);
    CpuDmaBuffer::new_zero(
        &device,
        NonZeroUsize::new(len).ok_or(BlkError::InvalidRequest)?,
        limits.dma_alignment,
        direction,
    )
    .map_err(BlkError::from)
}
