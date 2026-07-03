//! RDIF block-device adapter for [`crate::Cv181xSdhci`].

pub use rdif_block::{
    BInterface, BIrqHandler, BOwnedQueue, BQueue, BlkError, IQueue, IQueueOwned, Interface,
    OwnedRequest, PollError, QueueHandle, Request, RequestId as RdifRequestId,
    RequestPoll as OwnedRequestPoll, RequestStatus, SubmitError,
};
pub use sdmmc_protocol::rdif::{config::BlockConfig, device::BlockDevice, queue::BlockQueue};
use sdmmc_protocol::sdio::{card::SdioSdmmc, host2::SdioHost2Adapter};

use crate::Cv181xSdhci;

pub fn device(
    card: SdioSdmmc<SdioHost2Adapter<Cv181xSdhci>>,
    config: BlockConfig,
) -> BlockDevice<SdioHost2Adapter<Cv181xSdhci>> {
    BlockDevice::new(card, config)
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
    use sdmmc_protocol::rdif as protocol_rdif;

    use super::*;

    #[test]
    fn fifo_config_is_irq_driven_without_dma() {
        let config = fifo_config("cvsd", 16, true);
        let limits = protocol_rdif::queue_limits(&config, config.dma_mask);

        assert_eq!(config.name, "cvsd");
        assert_eq!(config.capacity_blocks, 16);
        assert!(config.irq_driven);
        assert!(!config.uses_dma());
        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, protocol_rdif::BLOCK_SIZE);
    }
}
