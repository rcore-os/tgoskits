use dma_api::{DmaConstraints, DmaDeviceInfo};

use crate::request::RequestFlags;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueLimits {
    /// Complete DMA identity and constraints of the physical device served by this queue.
    pub dma: DmaDeviceInfo,
    /// Required alignment of every device-visible segment length.
    pub dma_length_alignment: usize,
    pub max_inflight: usize,
    /// Maximum requests one native queue operation may stage before commit.
    pub max_submit_batch: usize,
    pub max_blocks_per_request: u32,
    pub max_segments: usize,
    pub supported_flags: RequestFlags,
    pub supports_flush: bool,
}

impl QueueLimits {
    pub const fn simple(logical_block_size: usize, dma: DmaDeviceInfo) -> Self {
        let device_constraints = dma.constraints();
        let align = if device_constraints.align > logical_block_size {
            device_constraints.align
        } else {
            logical_block_size
        };
        let max_segment_size = match device_constraints.max_segment_size {
            Some(max) if max < logical_block_size => max,
            _ => logical_block_size,
        };
        let constraints = DmaConstraints {
            align,
            max_segment_size: Some(max_segment_size),
            ..device_constraints
        };
        Self {
            dma: dma.with_constraints(constraints),
            dma_length_alignment: logical_block_size,
            max_inflight: 1,
            max_submit_batch: 1,
            max_blocks_per_request: 1,
            max_segments: 1,
            supported_flags: RequestFlags::NONE,
            supports_flush: false,
        }
    }
}

#[cfg(test)]
mod tests {
    use dma_api::{DmaCoherency, DmaDomainId};

    use super::*;

    #[test]
    fn physical_block_size_defaults_to_the_logical_block_size() {
        let default_geometry = DeviceInfo::new(16, 512);
        let native_geometry = default_geometry.with_physical_block_size(4096);

        assert_eq!(default_geometry.physical_block_size, 512);
        assert_eq!(native_geometry.logical_block_size, 512);
        assert_eq!(native_geometry.physical_block_size, 4096);
    }

    #[test]
    fn simple_limits_preserve_stricter_device_constraints() {
        let dma = DmaDeviceInfo::new(
            DmaDomainId::Direct,
            DmaCoherency::NonCoherent,
            DmaConstraints::new(0xffff)
                .with_align(1024)
                .with_boundary(4096)
                .with_max_segment_size(256),
        );

        let limits = QueueLimits::simple(512, dma);

        assert_eq!(limits.dma.domain(), DmaDomainId::Direct);
        assert_eq!(limits.dma.coherency(), DmaCoherency::NonCoherent);
        assert_eq!(
            limits.dma.constraints(),
            DmaConstraints {
                addr_mask: 0xffff,
                align: 1024,
                boundary: Some(4096),
                max_segment_size: Some(256),
            }
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QueueInfo {
    pub id: usize,
    pub device: DeviceInfo,
    pub limits: QueueLimits,
}
