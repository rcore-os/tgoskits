//! RDIF block-device adapter for [`crate::Cv181xSdhci`].

use dma_api::DeviceDma;
pub use rdif_block::{
    BInterface, BIrqControl, BIrqEndpoint, BQueue, BlkError, BlockIrqSource, CompletedRequest,
    CompletionSink, IQueue, Interface, OwnedRequest, QueueEventBatch, QueueExecution, QueueHandle,
    QueueKind, RequestId as RdifRequestId, ServiceProgress, SubmitError, SubmitOutcome,
};
pub use sdmmc_protocol::rdif::{config::BlockConfig, device::BlockDevice, queue::BlockQueue};
use sdmmc_protocol::sdio::{InitializedSdioCard, host2::SdioHost2Adapter};

use crate::Cv181xSdhci;

pub fn device(
    card: InitializedSdioCard<SdioHost2Adapter<Cv181xSdhci>>,
    config: BlockConfig,
) -> BlockDevice<SdioHost2Adapter<Cv181xSdhci>> {
    BlockDevice::from_initialized(card, config)
}

pub fn dma_config(name: &'static str, capacity_blocks: u64, dma: DeviceDma) -> BlockConfig {
    sdhci_host::rdif::dma_config(name, capacity_blocks, dma)
}

/// Build the FIFO-only configuration used while the controller initializes.
///
/// FIFO is confined to controller/card initialization; it cannot publish an
/// RDIF runtime queue.
pub const fn initialization_config(name: &'static str, capacity_blocks: u64) -> BlockConfig {
    BlockConfig::fifo(name, capacity_blocks)
}

#[cfg(test)]
mod tests {
    use sdmmc_protocol::rdif as protocol_rdif;

    use super::*;

    #[test]
    fn fifo_only_configuration_cannot_publish_runtime_queue() {
        let config = initialization_config("cvsd", 16);
        let limits = protocol_rdif::queue_limits(&config, config.dma_mask);

        assert_eq!(config.name, "cvsd");
        assert_eq!(config.capacity_blocks, 16);
        assert!(!config.uses_dma());
        assert!(!config.supports_runtime_queue());
        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, protocol_rdif::BLOCK_SIZE);
    }
}
