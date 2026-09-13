use alloc::sync::Arc;

use rdif_block::{ControlEvent, HardIrqHandler, IrqAck, IrqQueueMask};

use crate::{
    rdif::device::BlockInitStatus,
    sdio::host::{SdMmcIrqHandle, SdMmcIrqHost, block_queue_ready_from_host_event},
};

pub struct BlockIrqHandler<H>
where
    H: SdMmcIrqHost + 'static,
{
    pub(super) irq: H::IrqHandle,
    pub(super) init_status: Arc<BlockInitStatus>,
}

impl<H> HardIrqHandler for BlockIrqHandler<H>
where
    H: SdMmcIrqHost + 'static,
{
    fn ack(&mut self) -> IrqAck {
        let event = self.irq.handle_irq();
        let Some(queue_id) = block_queue_ready_from_host_event(&event) else {
            return IrqAck::spurious(0);
        };
        let control = if self.init_status.needs_controller_wake() {
            ControlEvent::new(0, 1)
        } else {
            ControlEvent::new(0, 0)
        };
        IrqAck::cleared(IrqQueueMask::from_queue(queue_id), control)
    }
}
