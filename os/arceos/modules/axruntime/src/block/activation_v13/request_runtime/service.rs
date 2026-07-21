//! Synchronous filesystem facade over the v0.13 ctx-to-hctx request path.

use rdif_block::{CompletedRequest, OwnedRequest, RequestFlags, RequestOp, validate_owned_request};

use super::{V13BlockDeviceView, V13SubmitErrorKind};
use crate::block::{
    BlockServiceError,
    service::{build_data_request, transfer_chunk_len, validate_data_transfer},
};

impl V13BlockDeviceView {
    /// Reads complete logical blocks through the immutable CPU-to-hctx map.
    pub fn read_blocks(
        &self,
        start_block: u64,
        destination: &mut [u8],
    ) -> Result<(), BlockServiceError> {
        validate_data_transfer(self.device_info(), start_block, destination.len())?;
        let mut completed_bytes = 0;
        while completed_bytes < destination.len() {
            let info = self.current_queue_info()?;
            let byte_len = transfer_chunk_len(info, destination.len() - completed_bytes)?;
            let lba = start_block + (completed_bytes / info.device.logical_block_size) as u64;
            let request = build_data_request(info, RequestOp::Read, lba, &[], byte_len)?;
            let completion = self.submit_and_wait(request)?;
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

    /// Writes complete logical blocks through the immutable CPU-to-hctx map.
    pub fn write_blocks(&self, start_block: u64, source: &[u8]) -> Result<(), BlockServiceError> {
        validate_data_transfer(self.device_info(), start_block, source.len())?;
        let mut completed_bytes = 0;
        while completed_bytes < source.len() {
            let info = self.current_queue_info()?;
            let byte_len = transfer_chunk_len(info, source.len() - completed_bytes)?;
            let lba = start_block + (completed_bytes / info.device.logical_block_size) as u64;
            let request = build_data_request(
                info,
                RequestOp::Write,
                lba,
                &source[completed_bytes..completed_bytes + byte_len],
                byte_len,
            )?;
            self.submit_and_wait(request)?.result?;
            completed_bytes += byte_len;
        }
        Ok(())
    }

    /// Flushes prior writes when the logical device advertises the command.
    pub fn flush(&self) -> Result<(), BlockServiceError> {
        let info = self.current_queue_info()?;
        if !info.limits.supports_flush {
            return Ok(());
        }
        let request = OwnedRequest {
            op: RequestOp::Flush,
            lba: 0,
            block_count: 0,
            data: None,
            flags: RequestFlags::NONE,
        };
        validate_owned_request(info, &request)?;
        self.submit_and_wait(request)?.result?;
        Ok(())
    }

    fn submit_and_wait(
        &self,
        request: OwnedRequest,
    ) -> Result<CompletedRequest, BlockServiceError> {
        let submitted = self.submit_owned(request).map_err(|error| {
            let (kind, _request) = error.into_parts();
            BlockServiceError::V13(kind)
        })?;
        submitted.wait().map_err(map_wait_error)
    }
}

fn map_wait_error(error: V13SubmitErrorKind) -> BlockServiceError {
    BlockServiceError::V13(error)
}
