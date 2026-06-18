//! RDIF block-device adapter for [`Sdhci`].

use core::{num::NonZeroUsize, ptr::NonNull};

use dma_api::DeviceDma;
pub use protocol_rdif::{BlockConfig, BlockDevice, BlockQueue};
pub use rdif_block::{
    BlkError, IQueue, Interface, Request, RequestId as RdifRequestId, RequestStatus,
};
use sdmmc_protocol::{BlockPoll, BlockRequestId, Error, rdif as protocol_rdif, sdio::SdioSdmmc};

use crate::{
    ADMA2_MAX_BLOCKS, ADMA2_MAX_TRANSFER_SIZE, BlockRequest, BlockRequestSlot, RequestId, Sdhci,
};

pub fn device(card: SdioSdmmc<Sdhci>, config: BlockConfig) -> BlockDevice<Sdhci> {
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

impl protocol_rdif::BlockHost for Sdhci {
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
    host: &mut Sdhci,
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
    host: &mut Sdhci,
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
            dma_api::DeviceDma::new(u32::MAX as u64, &TEST_DMA),
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
