use dma_api::DmaDomainId;

use crate::request::RequestFlags;

#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    pub num_blocks: u64,
    /// Addressing unit used by every request LBA and block count.
    pub logical_block_size: usize,
    /// Smallest device block that may require read-modify-write internally.
    pub physical_block_size: usize,
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
            physical_block_size: logical_block_size,
            read_only: false,
            name: None,
            vendor: None,
            model: None,
        }
    }

    /// Overrides the physical block size reported by the device.
    pub const fn with_physical_block_size(mut self, physical_block_size: usize) -> Self {
        self.physical_block_size = physical_block_size;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::DeviceInfo;

    #[test]
    fn physical_block_size_defaults_to_the_logical_block_size() {
        let default_geometry = DeviceInfo::new(16, 512);
        let native_geometry = default_geometry.with_physical_block_size(4096);

        assert_eq!(default_geometry.physical_block_size, 512);
        assert_eq!(native_geometry.logical_block_size, 512);
        assert_eq!(native_geometry.physical_block_size, 4096);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QueueLimits {
    pub dma_mask: u64,
    pub dma_domain: DmaDomainId,
    /// Required alignment of every device-visible segment start address.
    pub dma_alignment: usize,
    /// Required alignment of every device-visible segment length.
    pub dma_length_alignment: usize,
    /// Optional power-of-two boundary that one segment must not cross.
    pub segment_boundary: Option<usize>,
    pub max_inflight: usize,
    /// Maximum requests one native queue operation may stage before commit.
    pub max_submit_batch: usize,
    pub max_blocks_per_request: u32,
    pub max_segments: usize,
    pub max_segment_size: usize,
    pub supported_flags: RequestFlags,
    pub supports_flush: bool,
}

impl QueueLimits {
    pub const fn simple(logical_block_size: usize, dma_mask: u64) -> Self {
        Self {
            dma_mask,
            dma_domain: DmaDomainId::legacy_global(),
            dma_alignment: logical_block_size,
            dma_length_alignment: logical_block_size,
            segment_boundary: None,
            max_inflight: 1,
            max_submit_batch: 1,
            max_blocks_per_request: 1,
            max_segments: 1,
            max_segment_size: logical_block_size,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct QueueInfo {
    pub id: usize,
    pub device: DeviceInfo,
    pub limits: QueueLimits,
}
