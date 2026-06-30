//! RDIF block-device adapter for [`Sdhci`].

use dma_api::DeviceDma;
pub use protocol_rdif::{BlockConfig, BlockDevice, BlockQueue};
pub use rdif_block::{
    BInterface, BIrqHandler, BOwnedQueue, BQueue, BlkError, IQueue, IQueueOwned, Interface,
    OwnedRequest, PollError, QueueHandle, Request, RequestId as RdifRequestId,
    RequestPoll as OwnedRequestPoll, RequestStatus, SubmitError,
};
use sdmmc_protocol::{
    rdif as protocol_rdif,
    sdio::{SdioHost2Adapter, SdioSdmmc},
};

use crate::{ADMA2_MAX_BLOCKS, ADMA2_MAX_TRANSFER_SIZE, Sdhci};

pub fn device(
    card: SdioSdmmc<SdioHost2Adapter<Sdhci>>,
    config: BlockConfig,
) -> BlockDevice<SdioHost2Adapter<Sdhci>> {
    BlockDevice::new(card, config)
}

pub fn dma_config(
    name: &'static str,
    capacity_blocks: u64,
    irq_driven: bool,
    dma: DeviceDma,
) -> BlockConfig {
    BlockConfig::dma(name, capacity_blocks, irq_driven, dma)
        .with_max_blocks_per_request(ADMA2_MAX_BLOCKS)
        .with_max_segment_size(ADMA2_MAX_TRANSFER_SIZE)
}

pub const fn fifo_config(
    name: &'static str,
    capacity_blocks: u64,
    irq_driven: bool,
) -> BlockConfig {
    BlockConfig::fifo(name, capacity_blocks, irq_driven)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_config_keeps_one_block_limits() {
        let config = fifo_config("sdhci", 16, true);
        let limits = protocol_rdif::queue_limits(&config, config.dma_mask);

        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, protocol_rdif::BLOCK_SIZE);
        assert!(!config.uses_dma());
    }

    #[test]
    fn dma_config_advertises_adma_window() {
        let config = dma_config(
            "sdhci",
            16,
            true,
            dma_api::DeviceDma::new_legacy(u32::MAX as u64, &TEST_DMA),
        );
        let limits = protocol_rdif::queue_limits(&config, config.dma_mask);

        assert_eq!(limits.max_blocks_per_request, ADMA2_MAX_BLOCKS);
        assert_eq!(limits.max_segment_size, ADMA2_MAX_TRANSFER_SIZE);
        assert!(config.uses_dma());
    }

    struct TestDma;
    static TEST_DMA: TestDma = TestDma;

    impl dma_api::DmaOp for TestDma {
        fn page_size(&self) -> usize {
            protocol_rdif::BLOCK_SIZE
        }

        unsafe fn alloc_contiguous(
            &self,
            _constraints: dma_api::DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_contiguous(&self, _handle: dma_api::DmaAllocHandle) {}

        unsafe fn alloc_coherent(
            &self,
            _constraints: dma_api::DmaConstraints,
            _layout: core::alloc::Layout,
        ) -> Option<dma_api::DmaAllocHandle> {
            None
        }

        unsafe fn dealloc_coherent(&self, _handle: dma_api::DmaAllocHandle) {}

        unsafe fn map_streaming(
            &self,
            _constraints: dma_api::DmaConstraints,
            _addr: core::ptr::NonNull<u8>,
            _size: core::num::NonZeroUsize,
            _direction: dma_api::DmaDirection,
        ) -> Result<dma_api::DmaMapHandle, dma_api::DmaError> {
            Err(dma_api::DmaError::NoMemory)
        }

        unsafe fn unmap_streaming(&self, _handle: dma_api::DmaMapHandle) {}
    }
}
