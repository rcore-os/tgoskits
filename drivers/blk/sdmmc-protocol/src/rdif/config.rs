use rdif_block::{
    BlkError,
    dma_api::{self, DeviceDma},
};

use crate::Error;

pub const BLOCK_SIZE: usize = 512;
pub const DEFAULT_DMA_MASK: u64 = u32::MAX as u64;
pub const DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST: u32 = u16::MAX as u32 + 1;

/// Data path exposed after card initialization reaches `Ready`.
///
/// Initialization-only traffic may use controller FIFO helpers without
/// publishing a normal-I/O queue. Normal PIO is a different capability: it
/// owns request buffers and advances only from acknowledged IRQ snapshots.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockDataPath {
    InitializationOnly,
    Dma,
    InterruptPio,
}

#[derive(Clone)]
pub struct BlockConfig {
    pub name: &'static str,
    pub capacity_blocks: u64,
    pub dma_mask: u64,
    pub dma_domain: dma_api::DmaDomainId,
    pub max_blocks_per_request: u32,
    pub max_segment_size: usize,
    data_path: BlockDataPath,
}

impl BlockConfig {
    pub fn dma(name: &'static str, capacity_blocks: u64, dma: DeviceDma) -> Self {
        let dma_mask = dma.dma_mask();
        Self {
            name,
            capacity_blocks,
            dma_mask,
            dma_domain: dma.domain_id(),
            max_blocks_per_request: DEFAULT_DMA_MAX_BLOCKS_PER_REQUEST,
            max_segment_size: usize::MAX,
            data_path: BlockDataPath::Dma,
        }
    }

    /// Configuration used only by the bounded card-initialization state
    /// machine.
    ///
    /// This constructor deliberately does not expose a runtime queue. A
    /// `ControllerInit` separates bring-up traffic from normal interrupt-DMA
    /// or owned interrupt-PIO traffic; this value can never publish a polling
    /// queue.
    pub const fn fifo(name: &'static str, capacity_blocks: u64) -> Self {
        Self {
            name,
            capacity_blocks,
            dma_mask: DEFAULT_DMA_MASK,
            dma_domain: dma_api::DmaDomainId::legacy_global(),
            max_blocks_per_request: 1,
            max_segment_size: BLOCK_SIZE,
            data_path: BlockDataPath::InitializationOnly,
        }
    }

    /// Configuration for normal IRQ-driven programmed-I/O requests.
    ///
    /// The CPU buffer allocator still uses RDIF's DMA-shaped allocation
    /// contract, but the controller never observes its bus address. Using an
    /// unrestricted mask prevents a PIO queue from fabricating a hardware DMA
    /// address limitation.
    pub const fn interrupt_pio(name: &'static str, capacity_blocks: u64) -> Self {
        Self {
            name,
            capacity_blocks,
            dma_mask: u64::MAX,
            dma_domain: dma_api::DmaDomainId::legacy_global(),
            max_blocks_per_request: 1,
            max_segment_size: BLOCK_SIZE,
            data_path: BlockDataPath::InterruptPio,
        }
    }

    pub fn with_dma_mask(mut self, dma_mask: u64) -> Self {
        self.dma_mask = dma_mask;
        self
    }

    pub fn with_max_blocks_per_request(mut self, max_blocks_per_request: u32) -> Self {
        self.max_blocks_per_request = max_blocks_per_request;
        self
    }

    pub fn with_max_segment_size(mut self, max_segment_size: usize) -> Self {
        self.max_segment_size = max_segment_size;
        self
    }

    pub fn with_dma(mut self, dma: DeviceDma) -> Self {
        self.dma_mask = dma.dma_mask();
        self.dma_domain = dma.domain_id();
        self.data_path = BlockDataPath::Dma;
        self
    }

    pub const fn uses_dma(&self) -> bool {
        matches!(self.data_path, BlockDataPath::Dma)
    }

    pub const fn uses_interrupt_pio(&self) -> bool {
        matches!(self.data_path, BlockDataPath::InterruptPio)
    }

    pub const fn data_path(&self) -> BlockDataPath {
        self.data_path
    }

    /// Whether this configuration can expose the RDIF runtime queue.
    pub const fn supports_runtime_queue(&self) -> bool {
        matches!(
            self.data_path,
            BlockDataPath::Dma | BlockDataPath::InterruptPio
        )
    }
}

pub fn queue_limits(config: &BlockConfig, dma_mask: u64) -> rdif_block::QueueLimits {
    rdif_block::QueueLimits {
        dma_mask,
        dma_domain: config.dma_domain,
        dma_alignment: BLOCK_SIZE,
        max_inflight: 1,
        max_blocks_per_request: config.max_blocks_per_request,
        max_segments: 1,
        max_segment_size: config.max_segment_size,
        request_timeout_ns: rdif_block::DEFAULT_REQUEST_TIMEOUT_NS,
        supported_flags: rdif_block::RequestFlags::NONE,
        supports_flush: false,
        supports_discard: false,
        supports_write_zeroes: false,
    }
}

pub fn device_info(config: &BlockConfig) -> rdif_block::DeviceInfo {
    rdif_block::DeviceInfo {
        name: Some(config.name),
        ..rdif_block::DeviceInfo::new(config.capacity_blocks, BLOCK_SIZE)
    }
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
        Error::Timeout(_) => BlkError::TimedOut,
        Error::NoCard | Error::UnsupportedCommand | Error::CardLocked => BlkError::NotSupported,
        Error::Misaligned | Error::InvalidArgument => {
            BlkError::Other("SD/MMC request is not block aligned")
        }
        _ => BlkError::Io,
    }
}

pub fn can_fallback_to_fifo(err: Error) -> bool {
    matches!(
        err,
        Error::UnsupportedCommand | Error::InvalidArgument | Error::Misaligned
    )
}
