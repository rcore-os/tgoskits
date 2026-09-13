use alloc::sync::Arc;

use rdif_eth::{NetHardIrqHandler, NetHardIrqResult, NetIrqSnapshot};
use sdmmc_protocol::sdio::{HostEvent, HostEventKind, SdMmcIrqHandle};

use crate::rdif::device::IrqLatch;

pub(super) struct AicHardIrq<I: SdMmcIrqHandle> {
    irq: I,
    latch: Arc<IrqLatch>,
}

impl<I: SdMmcIrqHandle> AicHardIrq<I> {
    pub(super) fn new(irq: I, latch: Arc<IrqLatch>) -> Self {
        Self { irq, latch }
    }
}

impl<I: SdMmcIrqHandle> NetHardIrqHandler for AicHardIrq<I> {
    fn handle_irq(&mut self) -> NetHardIrqResult {
        let event = self.irq.handle_irq();
        if !self.latch.publish(&event) {
            return NetHardIrqResult::Spurious;
        }
        let snapshot = match event.kind() {
            HostEventKind::Error => NetIrqSnapshot::ERROR,
            HostEventKind::CardInterrupt => NetIrqSnapshot::RX,
            _ => NetIrqSnapshot::all_queue_work(),
        };
        NetHardIrqResult::Schedule(snapshot)
    }
}
