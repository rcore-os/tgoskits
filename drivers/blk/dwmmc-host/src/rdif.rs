//! RDIF block-device adapter for [`DwMmc`].

use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::DeviceDma;
pub use protocol_rdif::{BlockConfig, BlockDevice, BlockQueue};
pub use rdif_block::{
    BlkError, IQueue, Interface, Request, RequestId as RdifRequestId, RequestStatus,
};
use sdmmc_protocol::{BlockPoll, BlockRequestId, Error, rdif as protocol_rdif, sdio::SdioSdmmc};

use crate::{BlockRequest, BlockRequestSlot, DwMmc, RequestId};

pub fn device(card: SdioSdmmc<DwMmc>, config: BlockConfig) -> BlockDevice<DwMmc> {
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
        .with_max_segment_size(1024 * protocol_rdif::BLOCK_SIZE)
}

pub const fn fifo_config(
    name: &'static str,
    capacity_blocks: u64,
    irq_driven: bool,
) -> BlockConfig {
    BlockConfig::fifo(name, capacity_blocks, irq_driven)
}

impl protocol_rdif::BlockHost for DwMmc {
    type Request = BlockRequest;
    type Slot = BlockRequestSlot;

    fn submit_read_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, BlkError> {
        submit_request(
            self,
            Submission {
                start_block,
                buffer,
                size,
                dma,
                slot,
                pending,
                direction: Direction::Read,
            },
        )
    }

    fn submit_write_request(
        &mut self,
        start_block: u32,
        buffer: NonNull<u8>,
        size: NonZeroUsize,
        dma: Option<&DeviceDma>,
        slot: &mut Self::Slot,
        pending: &mut Option<Self::Request>,
    ) -> Result<BlockRequestId, BlkError> {
        submit_request(
            self,
            Submission {
                start_block,
                buffer,
                size,
                dma,
                slot,
                pending,
                direction: Direction::Write,
            },
        )
    }

    fn poll_block_request(
        &mut self,
        pending: &mut Option<Self::Request>,
        request: BlockRequestId,
        slot: &mut Self::Slot,
    ) -> Result<BlockPoll, Error> {
        self.poll_block_request(pending, RequestId::new(usize::from(request)), slot)
    }

    fn request_id(request: &Self::Request) -> BlockRequestId {
        BlockRequestId::new(usize::from(request.id()))
    }
}

#[derive(Clone, Copy)]
enum Direction {
    Read,
    Write,
}

struct Submission<'a> {
    start_block: u32,
    buffer: NonNull<u8>,
    size: NonZeroUsize,
    dma: Option<&'a DeviceDma>,
    slot: &'a mut BlockRequestSlot,
    pending: &'a mut Option<BlockRequest>,
    direction: Direction,
}

fn submit_request(
    host: &mut DwMmc,
    submission: Submission<'_>,
) -> Result<BlockRequestId, BlkError> {
    let Submission {
        start_block,
        buffer,
        size,
        dma,
        slot,
        pending,
        direction,
    } = submission;
    if pending.is_some() {
        return Err(BlkError::Retry);
    }
    let request = match submit_blocks(host, start_block, buffer, size, dma, slot, direction) {
        Ok(request) => request,
        Err(err) if dma.is_some() && protocol_rdif::can_fallback_to_fifo(err) => {
            submit_blocks(host, start_block, buffer, size, None, slot, direction)
                .map_err(protocol_rdif::map_dev_err_to_blk_err)?
        }
        Err(err) => return Err(protocol_rdif::map_dev_err_to_blk_err(err)),
    };
    let id = request.id();
    *pending = Some(request);
    Ok(BlockRequestId::new(usize::from(id)))
}

fn submit_blocks(
    host: &mut DwMmc,
    start_block: u32,
    buffer: NonNull<u8>,
    size: NonZeroUsize,
    dma: Option<&DeviceDma>,
    slot: &mut BlockRequestSlot,
    direction: Direction,
) -> Result<BlockRequest, Error> {
    let mode = protocol_rdif::transfer_mode_for_dma(dma);
    match direction {
        Direction::Read => host.submit_read_blocks(start_block, buffer, size, dma, mode, slot),
        Direction::Write => host.submit_write_blocks(start_block, buffer, size, dma, mode, slot),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fifo_config_keeps_one_block_limits() {
        let config = fifo_config("dwmmc", 16, true);
        let limits = protocol_rdif::queue_limits(&config, config.dma_mask);

        assert_eq!(limits.max_blocks_per_request, 1);
        assert_eq!(limits.max_segment_size, protocol_rdif::BLOCK_SIZE);
        assert!(!config.uses_dma());
    }
}
