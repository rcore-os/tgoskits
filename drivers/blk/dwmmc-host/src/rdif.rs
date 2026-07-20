//! RDIF block-device adapter for [`DwMmc`].

use dma_api::DeviceDma;
pub use rdif_block::{
    BInterface, BIrqControl, BIrqEndpoint, BQueue, BlkError, BlockIrqSource, CompletedRequest,
    CompletionSink, IQueue, Interface, OwnedRequest, QueueEventBatch, QueueExecution, QueueHandle,
    QueueKind, RequestId as RdifRequestId, ServiceProgress, SubmitError, SubmitOutcome,
};
pub use sdmmc_protocol::rdif::{config::BlockConfig, device::BlockDevice, queue::BlockQueue};
use sdmmc_protocol::{
    rdif::config as protocol_rdif_config,
    sdio::{InitializedSdioCard, host2::SdioHost2Adapter},
};

use crate::DwMmc;

pub fn device(
    card: InitializedSdioCard<SdioHost2Adapter<DwMmc>>,
    config: BlockConfig,
) -> BlockDevice<SdioHost2Adapter<DwMmc>> {
    BlockDevice::from_initialized(card, config)
}

pub fn dma_config(name: &'static str, capacity_blocks: u64, dma: DeviceDma) -> BlockConfig {
    BlockConfig::dma(name, capacity_blocks, dma)
        .with_max_blocks_per_request(1024)
        .with_max_segment_size(1024 * protocol_rdif_config::BLOCK_SIZE)
}

/// Build the FIFO-only configuration used by controller/card initialization.
///
/// FIFO transfers do not implement the owned interrupt-runtime contract, so
/// this value cannot publish an RDIF queue.
pub const fn initialization_config(name: &'static str, capacity_blocks: u64) -> BlockConfig {
    BlockConfig::fifo(name, capacity_blocks)
}

#[cfg(test)]
mod tests {
    use core::ptr::NonNull;

    use super::*;

    #[test]
    fn initialization_config_keeps_fifo_out_of_runtime() {
        let config = initialization_config("dwmmc", 16);
        let limits = protocol_rdif_config::queue_limits(&config, config.dma_mask);

        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, protocol_rdif_config::BLOCK_SIZE);
        assert!(!config.uses_dma());
        assert!(!config.supports_runtime_queue());
    }

    #[test]
    fn hardware_constructor_installs_typed_controller_lifecycle() {
        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let host = unsafe { DwMmc::new(base) };
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
        struct RuntimeClock;

        impl crate::HostClock for RuntimeClock {
            fn set_clock(&self, target_hz: u32) -> Result<u32, sdmmc_protocol::Error> {
                Ok(target_hz)
            }
        }

        let mut mmio = [0u32; 256];
        let base = NonNull::new(mmio.as_mut_ptr().cast()).unwrap();
        let mut host = unsafe { DwMmc::new(base) };
        host.set_external_clock(RuntimeClock);
        let mut host = SdioHost2Adapter::new(host);

        sdmmc_protocol::rdif::BlockHost::prepare_block_runtime(&mut host);

        assert!(
            host.with_host(|host| host.ext_clock.is_some()),
            "controller-owned clock capability must survive until detach or handoff"
        );
    }
}
