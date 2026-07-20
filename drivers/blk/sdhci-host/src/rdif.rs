//! RDIF block-device adapter for [`Sdhci`].

use dma_api::DeviceDma;
pub use rdif_block::{
    BInterface, BIrqControl, BIrqEndpoint, BQueue, BlkError, BlockIrqSource, CompletedRequest,
    CompletionSink, IQueue, Interface, OwnedRequest, QueueEventBatch, QueueExecution, QueueHandle,
    QueueKind, RequestId as RdifRequestId, ServiceProgress, SubmitError, SubmitOutcome,
};
#[cfg(test)]
use sdmmc_protocol::rdif::config as protocol_rdif_config;
pub use sdmmc_protocol::rdif::{config::BlockConfig, device::BlockDevice, queue::BlockQueue};
use sdmmc_protocol::sdio::{InitializedSdioCard, host2::SdioHost2Adapter};

use crate::{ADMA2_MAX_BLOCKS, ADMA2_MAX_TRANSFER_SIZE, Sdhci};

pub fn device(
    card: InitializedSdioCard<SdioHost2Adapter<Sdhci>>,
    config: BlockConfig,
) -> BlockDevice<SdioHost2Adapter<Sdhci>> {
    BlockDevice::from_initialized(card, config)
}

pub fn dma_config(name: &'static str, capacity_blocks: u64, dma: DeviceDma) -> BlockConfig {
    BlockConfig::dma(name, capacity_blocks, dma)
        .with_max_blocks_per_request(ADMA2_MAX_BLOCKS)
        .with_max_segment_size(ADMA2_MAX_TRANSFER_SIZE)
}

pub const fn fifo_config(name: &'static str, capacity_blocks: u64) -> BlockConfig {
    BlockConfig::interrupt_pio(name, capacity_blocks)
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use super::*;

    #[test]
    fn fifo_config_keeps_one_block_limits() {
        let config = fifo_config("sdhci", 16);
        let limits = protocol_rdif_config::queue_limits(&config, config.dma_mask);

        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, protocol_rdif_config::BLOCK_SIZE);
        assert!(!config.uses_dma());
        assert!(config.uses_interrupt_pio());
        assert!(
            config.supports_runtime_queue(),
            "an IRQ-backed FIFO controller must publish its normal-I/O queue"
        );
    }

    #[test]
    fn dma_config_advertises_adma_window() {
        let config = dma_config(
            "sdhci",
            16,
            dma_api::DeviceDma::new_legacy(u32::MAX as u64, &TEST_DMA),
        );
        let limits = protocol_rdif_config::queue_limits(&config, config.dma_mask);

        assert_eq!(limits.max_blocks_per_request, ADMA2_MAX_BLOCKS);
        assert_eq!(limits.max_segment_size, ADMA2_MAX_TRANSFER_SIZE);
        assert!(config.uses_dma());
    }

    #[test]
    fn hardware_constructor_installs_typed_controller_lifecycle() {
        #[repr(align(4))]
        struct FakeRegs([u8; 0x100]);

        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let host = unsafe { Sdhci::new(base) };
        let mut host = SdioHost2Adapter::new(host);
        sdmmc_protocol::rdif::BlockHost::prepare_block_runtime(&mut host);

        assert!(
            sdmmc_protocol::rdif::BlockHost::begin_recovery(
                &mut host,
                rdif_block::RecoveryCause::QueueFault { queue_id: 0 },
            )
            .is_ok()
        );
    }

    #[test]
    fn runtime_transition_retains_platform_clock_capability() {
        #[repr(align(4))]
        struct FakeRegs([u8; 0x100]);

        struct RuntimeClock;

        impl crate::HostClock for RuntimeClock {
            fn set_clock(&self, _target_hz: u32) -> Result<(), sdmmc_protocol::Error> {
                Ok(())
            }
        }

        let mut regs = FakeRegs([0; 0x100]);
        let base = NonNull::new(regs.0.as_mut_ptr()).unwrap();
        let mut host = unsafe { Sdhci::new(base) };
        host.set_external_clock(RuntimeClock);
        let mut host = SdioHost2Adapter::new(host);

        sdmmc_protocol::rdif::BlockHost::prepare_block_runtime(&mut host);

        assert!(
            host.with_host(|host| host.ext_clock.is_some()),
            "controller-owned clock capability must survive until detach or handoff"
        );
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
