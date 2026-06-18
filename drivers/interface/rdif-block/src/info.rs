use crate::request::RequestFlags;

#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    pub num_blocks: u64,
    pub logical_block_size: usize,
    pub read_only: bool,
    pub name: Option<&'static str>,
    pub vendor: Option<&'static str>,
    pub model: Option<&'static str>,
}

impl DeviceInfo {
    pub const fn new(num_blocks: u64, logical_block_size: usize) -> Self {
        Self {
            num_blocks,
            logical_block_size,
            read_only: false,
            name: None,
            vendor: None,
            model: None,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QueueLimits {
    pub dma_mask: u64,
    pub dma_alignment: usize,
    pub max_inflight: usize,
    pub max_blocks_per_request: u32,
    pub max_segments: usize,
    pub max_segment_size: usize,
    pub supported_flags: RequestFlags,
    pub supports_flush: bool,
    pub supports_discard: bool,
    pub supports_write_zeroes: bool,
}

impl QueueLimits {
    pub const fn simple(logical_block_size: usize, dma_mask: u64) -> Self {
        Self {
            dma_mask,
            dma_alignment: logical_block_size,
            max_inflight: 1,
            max_blocks_per_request: 1,
            max_segments: 1,
            max_segment_size: logical_block_size,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
            supports_discard: false,
            supports_write_zeroes: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QueueInfo {
    pub id: usize,
    pub device: DeviceInfo,
    pub limits: QueueLimits,
}
