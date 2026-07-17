use alloc::sync::Arc;

use crate::{
    rdif::{config::map_dev_err_to_blk_err, device::BlockControl, host::BlockHost},
    sdio::host::{
        DeferredIrqAck, HostEvent, HostEventKind, SdioIrqHandle,
        block_queue_ready_from_host_event,
    },
};

pub struct BlockIrqHandler<H>
where
    H: BlockHost,
{
    pub(super) irq: H::IrqHandle,
    pub(super) control: Option<Arc<BlockControl<H>>>,
}

impl<H> rdif_block::IrqHandler for BlockIrqHandler<H>
where
    H: BlockHost,
{
    fn handle_irq(&mut self) -> rdif_block::IrqOutcome {
        let host_event = self.irq.handle_irq();
        let mut event = rdif_block::Event::none();
        if let Some(queue_id) = block_queue_ready_from_host_event(&host_event) {
            event.push_queue(queue_id);
            if host_event.ack_deferred() {
                rdif_block::IrqOutcome::deferred(event.queues)
            } else {
                rdif_block::IrqOutcome::handled(event)
            }
        } else if host_event.kind() != HostEventKind::None {
            rdif_block::IrqOutcome::handled_control()
        } else {
            rdif_block::IrqOutcome::unhandled()
        }
    }

    fn continue_deferred_irq(&mut self) -> rdif_block::DeferredIrqProgress {
        let Some(control) = self.control.as_ref() else {
            return rdif_block::DeferredIrqProgress::Failed(rdif_block::BlkError::NotSupported);
        };
        let mut raw = match control.raw.try_borrow_mut() {
            Ok(raw) => raw,
            Err(_) => return rdif_block::DeferredIrqProgress::Deferred,
        };
        match H::acknowledge_deferred_irq(raw.host_mut()) {
            Ok(DeferredIrqAck::Unhandled) => rdif_block::DeferredIrqProgress::Unhandled,
            Ok(DeferredIrqAck::Contended) => rdif_block::DeferredIrqProgress::Deferred,
            Ok(DeferredIrqAck::Acknowledged) => {
                rdif_block::DeferredIrqProgress::Acknowledged(rdif_block::Event::from_queue_bits(1))
            }
            Err(error) => {
                rdif_block::DeferredIrqProgress::Failed(map_dev_err_to_blk_err(error))
            }
        }
    }
}
