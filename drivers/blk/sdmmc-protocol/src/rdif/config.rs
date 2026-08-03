use rdif_block::{BlkError, DeviceInfo, QueueLimits, RequestFlags, dma_api::DeviceDma};

use crate::Error;

pub const BLOCK_SIZE: usize = 512;
pub const DEFAULT_DMA_MASK: u64 = u32::MAX as u64;
pub const DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST: u32 = u16::MAX as u32 + 1;

/// Immutable SD/MMC block geometry and hardware queue constraints.
///
/// DMA allocation ownership stays with the physical host. This value is safe
/// to copy into the controller and hardware queue without cloning a
/// [`DeviceDma`] capability.
#[derive(Clone, Copy)]
pub struct BlockConfig {
    pub device: DeviceInfo,
    pub limits: QueueLimits,
}

impl BlockConfig {
    pub fn dma(name: &'static str, capacity_blocks: u64, dma: &DeviceDma) -> Self {
        Self {
            device: DeviceInfo {
                name: Some(name),
                ..DeviceInfo::new(capacity_blocks, BLOCK_SIZE)
            },
            limits: QueueLimits {
                dma_mask: dma.dma_mask(),
                dma_domain: dma.domain_id(),
                dma_alignment: BLOCK_SIZE,
                dma_length_alignment: BLOCK_SIZE,
                segment_boundary: None,
                max_inflight: 1,
                max_submit_batch: 1,
                max_blocks_per_request: DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST,
                max_segments: 1,
                max_segment_size: usize::MAX,
                supported_flags: RequestFlags::NONE,
                supports_flush: false,
            },
        }
    }

    pub fn with_dma_mask(mut self, dma_mask: u64) -> Self {
        self.limits.dma_mask = dma_mask;
        self
    }

    pub fn with_max_blocks_per_request(mut self, max_blocks_per_request: u32) -> Self {
        self.limits.max_blocks_per_request = max_blocks_per_request;
        self
    }

    pub fn with_max_segment_size(mut self, max_segment_size: usize) -> Self {
        self.limits.max_segment_size = max_segment_size;
        self
    }

    pub fn with_segment_boundary(mut self, segment_boundary: usize) -> Self {
        self.limits.segment_boundary = Some(segment_boundary);
        self
    }

    pub const fn uses_dma(&self) -> bool {
        true
    }

    pub const fn name(&self) -> &'static str {
        match self.device.name {
            Some(name) => name,
            None => "sdmmc",
        }
    }

    pub const fn capacity_blocks(&self) -> u64 {
        self.device.num_blocks
    }

    pub fn set_capacity_blocks(&mut self, capacity_blocks: u64) {
        self.device.num_blocks = capacity_blocks;
    }
}

pub const fn queue_limits(config: &BlockConfig) -> QueueLimits {
    config.limits
}

pub const fn device_info(config: &BlockConfig) -> DeviceInfo {
    config.device
}

pub fn block_addr_for_card(block_id: u64, high_capacity: bool) -> Result<u32, BlkError> {
    let block_id = u32::try_from(block_id).map_err(|_| BlkError::InvalidBlockIndex(block_id))?;
    if high_capacity {
        Ok(block_id)
    } else {
        block_id
            .checked_mul(BLOCK_SIZE as u32)
            .ok_or(BlkError::InvalidBlockIndex(block_id as u64))
    }
}

pub fn map_dev_err_to_blk_err(err: Error) -> BlkError {
    match err {
        Error::Busy => BlkError::Retry,
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => BlkError::NotSupported,
        Error::Misaligned | Error::InvalidArgument => {
            BlkError::Other("SD/MMC request is not block aligned")
        }
        _ => BlkError::Io,
    }
}
