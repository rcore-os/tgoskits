//! Owned-DMA, IRQ-driven block adapter for [`Sdhci`].

use dma_api::DeviceDma;
pub use rdif_block::{
    BlkError, BlockController, CompletedRequest, ControllerEvent, ControllerState,
    ControllerUpdate, HardIrqHandler, HardwareQueue, OwnedRequest, QueueInfo, QueueLimits,
    RequestFlags, RequestId as RdifRequestId, RequestOp, SubmitError,
};
use sdmmc_host::HostParts;
#[cfg(test)]
use sdmmc_protocol::rdif::config as protocol_rdif_config;
pub use sdmmc_protocol::rdif::{config::BlockConfig, device::BlockDevice, queue::BlockQueue};
use sdmmc_protocol::sdio::native::SdMmcCard;

use crate::{ADMA2_MAX_BLOCKS, ADMA2_MAX_TRANSFER_SIZE, DWC_MSHC_ADMA_BOUNDARY, Sdhci};

pub fn device(
    parts: HostParts<Sdhci, crate::SdhciIrqHandle, crate::SdhciCardIrqHandle>,
    config: BlockConfig,
) -> BlockDevice<Sdhci> {
    BlockDevice::new(SdMmcCard::new(parts.bus), parts.irq, config)
}

pub fn initializing_device(
    parts: HostParts<Sdhci, crate::SdhciIrqHandle, crate::SdhciCardIrqHandle>,
    config: BlockConfig,
    preference: sdmmc_protocol::sdio::init::CardInitPreference,
) -> BlockDevice<Sdhci> {
    BlockDevice::new_initializing(SdMmcCard::new(parts.bus), parts.irq, config, preference)
}

pub fn dma_config(name: &'static str, capacity_blocks: u64, dma: &DeviceDma) -> BlockConfig {
    BlockConfig::dma(name, capacity_blocks, dma)
        .with_max_blocks_per_request(ADMA2_MAX_BLOCKS)
        .with_max_segment_size(ADMA2_MAX_TRANSFER_SIZE)
        .with_segment_boundary(DWC_MSHC_ADMA_BOUNDARY)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dma_config_advertises_adma_window() {
        let dma = dma_api::DeviceDma::new(
            dma_api::DmaDeviceInfo::new(
                dma_api::DmaDomainId::Direct,
                dma_api::DmaCoherency::NonCoherent,
                dma_api::DmaConstraints::new(u32::MAX as u64),
            ),
            &TEST_DMA,
        );
        let config = dma_config("sdhci", 16, &dma);
        let limits = protocol_rdif_config::queue_limits(&config);

        assert_eq!(limits.max_blocks_per_request, ADMA2_MAX_BLOCKS);
        assert_eq!(
            limits.dma.constraints().max_segment_size,
            Some(ADMA2_MAX_TRANSFER_SIZE)
        );
        assert_eq!(
            limits.dma.constraints().boundary,
            Some(DWC_MSHC_ADMA_BOUNDARY)
        );
        assert_eq!(limits.max_inflight, 1);
        assert!(config.uses_dma());
    }

    struct TestDma;
    static TEST_DMA: TestDma = TestDma;

    impl dma_api::DmaOp for TestDma {
        fn page_size(&self) -> usize {
            protocol_rdif_config::BLOCK_SIZE
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

        unsafe fn dealloc_coherent(
            &self,
            _handle: dma_api::DmaAllocHandle,
        ) -> Result<(), dma_api::DmaError> {
            Ok(())
        }

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
