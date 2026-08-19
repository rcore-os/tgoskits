use core::num::NonZeroUsize;

use dma_api::{CompletedDma, CpuDmaBuffer, DeviceDma, DmaDirection, PreparedDma};
use rdif_block::{BlkError, QueueLimits};

use crate::os::dma_op;

pub(super) fn prepare_read(limits: QueueLimits, len: usize) -> Result<PreparedDma, BlkError> {
    allocate(limits, len, DmaDirection::FromDevice).map(CpuDmaBuffer::prepare_for_device)
}

#[cfg(any(feature = "ext4", feature = "fat"))]
pub(super) fn prepare_write(limits: QueueLimits, source: &[u8]) -> Result<PreparedDma, BlkError> {
    let mut buffer = allocate(limits, source.len(), DmaDirection::ToDevice)?;
    buffer.copy_from_slice_cpu(source);
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
    let constraints = limits.dma.constraints();
    if constraints.align == 0
        || limits.dma_length_alignment == 0
        || !len.is_multiple_of(limits.dma_length_alignment)
    {
        return Err(BlkError::InvalidRequest);
    }
    let dma_op = dma_op().ok_or(BlkError::Io)?;
    if let Some(boundary) = constraints.boundary
        && !boundary.is_power_of_two()
    {
        return Err(BlkError::InvalidRequest);
    }
    let device = DeviceDma::new(limits.dma, dma_op);
    CpuDmaBuffer::new_zero(
        &device,
        NonZeroUsize::new(len).ok_or(BlkError::InvalidRequest)?,
        constraints.align,
        direction,
    )
    .map_err(BlkError::from)
}
