use crate::{
    rdif::host::BlockHost,
    sdio::host::{SdioIrqHandle, block_queue_ready_from_host_event},
};

pub struct BlockIrqHandler<H>
where
    H: BlockHost,
{
    pub(super) irq: H::IrqHandle,
}

impl<H> rdif_block::IrqHandler for BlockIrqHandler<H>
where
    H: BlockHost,
{
    fn handle_irq(&mut self) -> rdif_block::Event {
        let host_event = self.irq.handle_irq();
        let mut event = rdif_block::Event::none();
        if let Some(queue_id) = block_queue_ready_from_host_event(&host_event) {
            event.push_queue(queue_id);
        }
        event
    }
}
