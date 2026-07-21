//! Runtime facade for call-stack-only software block devices.

use alloc::string::String;

use ax_driver::block::RegisteredInlineBlockDevice;
use ax_kspin::SpinNoPreempt;
use rdif_block::{
    CompletedRequest, DeviceInfo, HardwareQueueLimits, OwnedRequest, QueueExecution, QueueInfo,
    QueueKind, QueueLimits, RequestFlags, RequestId, RequestOp, validate_owned_request,
};

use super::{
    BlockServiceError,
    service::{build_data_request, transfer_chunk_len, validate_data_transfer},
};

/// Synchronous facade that never allocates an asynchronous request identity.
pub struct InlineBlockDeviceView {
    name: String,
    device_info: DeviceInfo,
    queue_info: QueueInfo,
    device: SpinNoPreempt<RegisteredInlineBlockDevice>,
}

impl InlineBlockDeviceView {
    /// Takes the move-only discovery owner into the runtime.
    pub fn new(device: RegisteredInlineBlockDevice) -> Self {
        let name = String::from(device.name());
        let device_info = device.device_info();
        let queue_info = inline_queue_info(device_info, device.limits());
        Self {
            name,
            device_info,
            queue_info,
            device: SpinNoPreempt::new(device),
        }
    }

    /// Returns the stable registry name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns immutable logical geometry.
    pub const fn device_info(&self) -> DeviceInfo {
        self.device_info
    }

    /// Executes a read entirely in the calling task context.
    pub fn read_blocks(
        &self,
        start_block: u64,
        destination: &mut [u8],
    ) -> Result<(), BlockServiceError> {
        validate_data_transfer(self.device_info, start_block, destination.len())?;
        let mut completed_bytes = 0;
        while completed_bytes < destination.len() {
            let byte_len =
                transfer_chunk_len(self.queue_info, destination.len() - completed_bytes)?;
            let lba = start_block + (completed_bytes / self.device_info.logical_block_size) as u64;
            let request = build_data_request(self.queue_info, RequestOp::Read, lba, &[], byte_len)?;
            let completion = self.execute(request)?;
            completion.result?;
            let data = completion
                .request
                .data
                .ok_or(BlockServiceError::DriverInvariant)?;
            data.copy_from_device_to_slice(
                &mut destination[completed_bytes..completed_bytes + byte_len],
            );
            completed_bytes += byte_len;
        }
        Ok(())
    }

    /// Executes a write entirely in the calling task context.
    pub fn write_blocks(&self, start_block: u64, source: &[u8]) -> Result<(), BlockServiceError> {
        validate_data_transfer(self.device_info, start_block, source.len())?;
        let mut completed_bytes = 0;
        while completed_bytes < source.len() {
            let byte_len = transfer_chunk_len(self.queue_info, source.len() - completed_bytes)?;
            let lba = start_block + (completed_bytes / self.device_info.logical_block_size) as u64;
            let request = build_data_request(
                self.queue_info,
                RequestOp::Write,
                lba,
                &source[completed_bytes..completed_bytes + byte_len],
                byte_len,
            )?;
            self.execute(request)?.result?;
            completed_bytes += byte_len;
        }
        Ok(())
    }

    /// Flushes inline state when the software device advertises the command.
    pub fn flush(&self) -> Result<(), BlockServiceError> {
        if !self.queue_info.limits.supports_flush {
            return Ok(());
        }
        let request = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        self.execute(request)?.result?;
        Ok(())
    }

    fn execute(&self, request: OwnedRequest) -> Result<CompletedRequest, BlockServiceError> {
        validate_owned_request(self.queue_info, &request)?;
        let completion = self.device.lock().execute_owned(request);
        if completion.id != RequestId::INLINE {
            return Err(BlockServiceError::DriverInvariant);
        }
        Ok(completion)
    }
}

fn inline_queue_info(device: DeviceInfo, limits: HardwareQueueLimits) -> QueueInfo {
    QueueInfo {
        id: 0,
        device,
        limits: QueueLimits {
            dma_mask: limits.dma_mask,
            dma_domain: limits.dma_domain,
            dma_alignment: limits.dma_alignment,
            max_inflight: 1,
            max_blocks_per_request: limits.max_blocks_per_request,
            max_segments: limits.max_segments,
            max_segment_size: limits.max_segment_size,
            request_timeout_ns: 0,
            supported_flags: limits.supported_flags,
            supports_flush: limits.supports_flush,
            supports_discard: limits.supports_discard,
            supports_write_zeroes: limits.supports_write_zeroes,
        },
        kind: QueueKind::Inline,
        execution: QueueExecution::Inline,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_limits_do_not_install_an_async_watchdog_or_credit_pool() {
        let info = inline_queue_info(
            DeviceInfo::new(8, 512),
            HardwareQueueLimits::simple(512, u64::MAX),
        );

        assert_eq!(info.kind, QueueKind::Inline);
        assert_eq!(info.execution, QueueExecution::Inline);
        assert_eq!(info.limits.max_inflight, 1);
        assert_eq!(info.limits.request_timeout_ns, 0);
    }
}
