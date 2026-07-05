//! RDIF block-device adapter for [`PhytiumMci`].

use dma_api::DeviceDma;
pub use rdif_block::{
    BInterface, BIrqHandler, BOwnedQueue, BQueue, BlkError, IQueue, IQueueOwned, Interface,
    OwnedRequest, PollError, QueueHandle, Request, RequestId as RdifRequestId,
    RequestPoll as OwnedRequestPoll, RequestStatus, SubmitError,
};
pub use sdmmc_protocol::rdif::{config::BlockConfig, device::BlockDevice, queue::BlockQueue};
use sdmmc_protocol::{
    rdif::config as protocol_rdif_config,
    sdio::{card::SdioSdmmc, host2::SdioHost2Adapter},
};

use crate::PhytiumMci;

pub fn device(
    card: SdioSdmmc<SdioHost2Adapter<PhytiumMci>>,
    config: BlockConfig,
) -> BlockDevice<SdioHost2Adapter<PhytiumMci>> {
    BlockDevice::new(card, config)
}

pub fn dma_config(
    name: &'static str,
    capacity_blocks: u64,
    irq_driven: bool,
    dma: DeviceDma,
) -> BlockConfig {
    BlockConfig::dma(name, capacity_blocks, irq_driven, dma)
        .with_max_blocks_per_request(1024)
        .with_max_segment_size(1024 * protocol_rdif_config::BLOCK_SIZE)
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
        let config = fifo_config("phytium-mci", 16, true);
        let limits = protocol_rdif_config::queue_limits(&config, config.dma_mask);

        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, protocol_rdif_config::BLOCK_SIZE);
        assert!(!config.uses_dma());
    }
}
