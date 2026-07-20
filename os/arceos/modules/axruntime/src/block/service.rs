//! Filesystem-facing synchronous service over inline or IRQ-only queues.

use core::{num::NonZeroUsize, sync::atomic::Ordering};

use dma_api::{CpuDmaBuffer, DeviceDma, DmaDirection};
use rdif_block::{
    BlkError, CompletedRequest, OwnedRequest, QueueInfo, RequestFlags, RequestId, RequestOp,
    SubmitOutcome, validate_owned_request,
};
use thiserror::Error;

use super::{BlockDeviceView, HardwareQueueError};
use crate::block::controller::RuntimeQueue;

/// Failure of one runtime-owned synchronous block operation.
#[derive(Debug, Error)]
pub enum BlockServiceError {
    /// Geometry, range, or queue limits reject the requested transfer.
    #[error("invalid block service transfer")]
    InvalidTransfer,
    /// DMA backing could not be constructed for the selected queue.
    #[error("block DMA allocation or mapping failed: {0}")]
    Dma(#[from] dma_api::DmaError),
    /// The portable driver rejected or failed the operation.
    #[error(transparent)]
    Driver(#[from] BlkError),
    /// The shared hardware queue rejected runtime ownership or scheduling.
    #[error(transparent)]
    HardwareQueue(#[from] HardwareQueueError),
    /// A driver violated its declared inline/interrupt ownership contract.
    #[error("block driver violated its queue completion contract")]
    DriverInvariant,
    /// Controller admission closed for recovery, shutdown, or passthrough.
    #[error("block controller is not available for host service")]
    ControllerUnavailable,
    /// A controller-only compatibility caller did not identify one disk.
    #[error("block controller owns {device_count} logical devices; select a device view")]
    AmbiguousLogicalDevice { device_count: usize },
}

impl BlockDeviceView {
    /// Reads complete logical blocks and waits only for directed IRQ
    /// completion when the selected queue is interrupt-backed.
    pub fn read_blocks(
        &self,
        start_block: u64,
        destination: &mut [u8],
    ) -> Result<(), BlockServiceError> {
        let _operation = self
            .controller()
            .begin_operation()
            .ok_or(BlockServiceError::ControllerUnavailable)?;
        self.validate_data_transfer(start_block, destination.len())?;
        let mut completed_bytes = 0;
        while completed_bytes < destination.len() {
            let queue = self.select_queue(RequestOp::Read)?;
            let info = queue.info();
            let byte_len = transfer_chunk_len(info, destination.len() - completed_bytes)?;
            let lba = start_block + (completed_bytes / info.device.logical_block_size) as u64;
            let request = build_data_request(info, RequestOp::Read, lba, &[], byte_len)?;
            let completion = submit_and_wait(queue, request)?;
            completion.result?;
            let data = completion
                .request
                .data
                .ok_or(BlockServiceError::DriverInvariant)?;
            data.copy_from_device_to_slice(
                &mut destination[completed_bytes..completed_bytes + byte_len],
            );
            self.runtime_device()
                .record_successful_io(RequestOp::Read, byte_len);
            completed_bytes += byte_len;
        }
        Ok(())
    }

    /// Writes complete logical blocks and waits only for directed IRQ
    /// completion when the selected queue is interrupt-backed.
    pub fn write_blocks(&self, start_block: u64, source: &[u8]) -> Result<(), BlockServiceError> {
        let _operation = self
            .controller()
            .begin_operation()
            .ok_or(BlockServiceError::ControllerUnavailable)?;
        self.validate_data_transfer(start_block, source.len())?;
        let mut completed_bytes = 0;
        while completed_bytes < source.len() {
            let queue = self.select_queue(RequestOp::Write)?;
            let info = queue.info();
            let byte_len = transfer_chunk_len(info, source.len() - completed_bytes)?;
            let lba = start_block + (completed_bytes / info.device.logical_block_size) as u64;
            let request = build_data_request(
                info,
                RequestOp::Write,
                lba,
                &source[completed_bytes..completed_bytes + byte_len],
                byte_len,
            )?;
            let completion = submit_and_wait(queue, request)?;
            completion.result?;
            self.runtime_device()
                .record_successful_io(RequestOp::Write, byte_len);
            completed_bytes += byte_len;
        }
        Ok(())
    }

    /// Flushes prior writes when the driver advertises a flush command.
    pub fn flush(&self) -> Result<(), BlockServiceError> {
        let _operation = self
            .controller()
            .begin_operation()
            .ok_or(BlockServiceError::ControllerUnavailable)?;
        let device = self.runtime_device();
        let Some(queue) = device
            .queues
            .iter()
            .find(|queue| queue.info().limits.supports_flush)
        else {
            return Ok(());
        };
        let request = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        validate_owned_request(queue.info(), &request)?;
        submit_and_wait(queue, request)?.result?;
        Ok(())
    }

    fn validate_data_transfer(
        &self,
        start_block: u64,
        byte_len: usize,
    ) -> Result<(), BlockServiceError> {
        let device_info = self.runtime_device().info();
        let block_size = device_info.logical_block_size;
        if byte_len == 0 || block_size == 0 || !byte_len.is_multiple_of(block_size) {
            return Err(BlockServiceError::InvalidTransfer);
        }
        let blocks =
            u64::try_from(byte_len / block_size).map_err(|_| BlockServiceError::InvalidTransfer)?;
        if start_block
            .checked_add(blocks)
            .is_none_or(|end| end > device_info.num_blocks)
        {
            return Err(BlockServiceError::InvalidTransfer);
        }
        Ok(())
    }

    fn select_queue(&self, operation: RequestOp) -> Result<&RuntimeQueue, BlockServiceError> {
        let device = self.runtime_device();
        let start = device.dispatch_cursor.fetch_add(1, Ordering::Relaxed);
        (0..device.queues.len())
            .map(|offset| &device.queues[(start + offset) % device.queues.len()])
            .find(|queue| queue_supports(queue.info(), operation))
            .ok_or(BlockServiceError::Driver(BlkError::NotSupported))
    }
}

fn submit_and_wait(
    queue: &RuntimeQueue,
    request: OwnedRequest,
) -> Result<CompletedRequest, BlockServiceError> {
    match queue {
        RuntimeQueue::Inline(queue) => submit_inline(queue, request),
        RuntimeQueue::Interrupt(queue) => match queue.submit_owned(request) {
            Ok(submitted) => Ok(submitted.wait()?),
            Err(error) => {
                let (runtime_error, _request) = error.into_parts();
                Err(runtime_error.into())
            }
        },
    }
}

fn submit_inline(
    queue: &super::controller::InlineQueue,
    request: OwnedRequest,
) -> Result<CompletedRequest, BlockServiceError> {
    // The queue gate serializes the complete call-stack-only ownership round
    // trip. Inline queues have no waiter, tag, generation, or completion table;
    // their reserved sentinel can never alias an interrupt request identity.
    let id = RequestId::INLINE;
    let mut driver = queue.queue.lock();
    if !queue.available.load(Ordering::Acquire) {
        return Err(BlockServiceError::ControllerUnavailable);
    }
    let outcome = driver
        .as_mut()
        .ok_or(BlockServiceError::ControllerUnavailable)?
        .submit_owned(id, request);
    let violates_inline_contract = match &outcome {
        Ok(SubmitOutcome::Completed(completion)) => completion.id != id,
        Ok(SubmitOutcome::Queued) => true,
        Err(error) => error.id() != id,
    };
    if violates_inline_contract {
        // Publish the poison while still holding the queue gate. A submitter
        // that observed the old value before blocking on this gate must recheck
        // after acquisition and cannot enter the driver after this violation.
        queue.available.store(false, Ordering::Release);
    }
    drop(driver);
    match outcome {
        Ok(SubmitOutcome::Completed(completion)) if completion.id == id => Ok(completion),
        Ok(SubmitOutcome::Completed(mut completion)) => {
            completion.result = Err(BlkError::Io);
            // Inline completion means the call already returned all hardware
            // ownership synchronously. A wrong identity poisons the endpoint,
            // but ordinary Rust Drop is correct for the returned CPU buffer.
            drop(completion);
            Err(contain_inline_contract_violation(queue))
        }
        Ok(SubmitOutcome::Queued) => Err(contain_inline_contract_violation(queue)),
        Err(error) => {
            let (returned_id, driver_error, request) = error.into_parts();
            if returned_id != id {
                // SubmitError explicitly means the driver did not accept the
                // request, so no device can still observe its backing memory.
                drop(request);
                return Err(contain_inline_contract_violation(queue));
            }
            drop(request);
            Err(driver_error.into())
        }
    }
}

fn contain_inline_contract_violation(queue: &super::controller::InlineQueue) -> BlockServiceError {
    // QueueHandle owns the one-shot Live -> Attempted -> Closed transaction.
    // If driver shutdown fails, the handle retains its endpoint permanently;
    // later teardown observes Attempted and cannot re-enter untrusted code.
    let _shutdown_result = queue.shutdown_after_contract_violation();
    BlockServiceError::DriverInvariant
}

fn build_data_request(
    info: QueueInfo,
    operation: RequestOp,
    lba: u64,
    source: &[u8],
    byte_len: usize,
) -> Result<OwnedRequest, BlockServiceError> {
    let direction = match operation {
        RequestOp::Read => DmaDirection::FromDevice,
        RequestOp::Write => DmaDirection::ToDevice,
        _ => return Err(BlockServiceError::InvalidTransfer),
    };
    let dma = DeviceDma::new(
        info.limits.dma_domain,
        info.limits.dma_mask,
        axklib::dma::op(),
    );
    let mut data = CpuDmaBuffer::new_zero(
        &dma,
        NonZeroUsize::new(byte_len).ok_or(BlockServiceError::InvalidTransfer)?,
        info.limits
            .dma_alignment
            .max(info.device.logical_block_size),
        direction,
    )?;
    if operation == RequestOp::Write {
        data.copy_to_device_from_slice(source);
    }
    let request = OwnedRequest {
        op: operation,
        lba,
        block_count: u32::try_from(byte_len / info.device.logical_block_size)
            .map_err(|_| BlockServiceError::InvalidTransfer)?,
        data: Some(data),
        flags: RequestFlags::NONE,
    };
    validate_owned_request(info, &request)?;
    Ok(request)
}

fn transfer_chunk_len(info: QueueInfo, remaining: usize) -> Result<usize, BlockServiceError> {
    let block_size = info.device.logical_block_size;
    let max_blocks = usize::try_from(info.limits.max_blocks_per_request)
        .map_err(|_| BlockServiceError::InvalidTransfer)?;
    let limit = max_blocks
        .checked_mul(block_size)
        .map(|bytes| bytes.min(info.limits.max_segment_size))
        .ok_or(BlockServiceError::InvalidTransfer)?;
    let aligned_limit = limit - limit % block_size;
    if aligned_limit == 0 {
        return Err(BlockServiceError::InvalidTransfer);
    }
    Ok(remaining.min(aligned_limit))
}

fn queue_supports(info: QueueInfo, operation: RequestOp) -> bool {
    match operation {
        RequestOp::Read => true,
        RequestOp::Write => !info.device.read_only,
        RequestOp::Flush => info.limits.supports_flush,
        RequestOp::Discard => info.limits.supports_discard,
        RequestOp::WriteZeroes => info.limits.supports_write_zeroes,
    }
}

#[cfg(test)]
mod tests {
    use rdif_block::{DeviceInfo, IdList, QueueExecution, QueueKind, QueueLimits};

    use super::*;

    #[test]
    fn transfer_chunk_is_block_aligned_and_bounded_by_queue_limits() {
        let mut limits = QueueLimits::simple(512, u64::MAX);
        limits.max_blocks_per_request = 8;
        limits.max_segment_size = 3 * 512;
        let mut sources = IdList::none();
        sources.insert(0);
        let info = QueueInfo {
            id: 0,
            device: DeviceInfo::new(64, 512),
            limits,
            kind: QueueKind::Interrupt { sources },
            execution: QueueExecution::Tagged,
        };

        assert_eq!(transfer_chunk_len(info, 9 * 512).unwrap(), 3 * 512);
    }
}
