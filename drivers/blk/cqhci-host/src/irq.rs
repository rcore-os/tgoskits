//! Hard-IRQ acknowledgement for a CQHCI engine sharing an SD/MMC IRQ line.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rdif_block::{ControlEvent, HardIrqHandler, IrqAck, IrqQueueMask};
use sdmmc_protocol::sdio::host::{HostEvent, HostEventKind, SdMmcIrqHandle};

use crate::{read_u32, regs::*, write_u32};

pub(crate) struct IrqState {
    base: usize,
    active: AtomicBool,
    mailbox: AtomicU64,
}

impl IrqState {
    pub(crate) fn new(base: usize) -> Self {
        Self {
            base,
            active: AtomicBool::new(false),
            mailbox: AtomicU64::new(0),
        }
    }

    pub(crate) fn set_active(&self, active: bool) {
        self.active.store(active, Ordering::Release);
    }

    fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(crate) fn take(&self) -> (u32, u32) {
        let snapshot = self.mailbox.swap(0, Ordering::AcqRel);
        (snapshot as u32, (snapshot >> 32) as u32)
    }

    fn acknowledge(&self, legacy_error: bool) -> bool {
        if !self.is_active() {
            return false;
        }
        let hardware_status = read_u32(self.base, IS);
        if hardware_status == 0 && !legacy_error {
            return false;
        }
        write_u32(self.base, IS, hardware_status);
        let completed = if hardware_status & IS_TASK_COMPLETE != 0 {
            let completed = read_u32(self.base, TCN);
            write_u32(self.base, TCN, completed);
            completed
        } else {
            0
        };
        // Publish errors and tag completions from the same observation as a
        // single atomic snapshot, before the runtime wakeup.
        let status = hardware_status | if legacy_error { IS_RESPONSE_ERROR } else { 0 };
        self.mailbox.fetch_or(
            u64::from(status) | (u64::from(completed) << 32),
            Ordering::Release,
        );
        true
    }
}

/// IRQ endpoint that acknowledges both legacy SDHCI and CQHCI state.
pub struct CqhciIrqHandle<L> {
    legacy: L,
    state: Arc<IrqState>,
}

impl<L> CqhciIrqHandle<L> {
    pub(crate) fn new(legacy: L, state: Arc<IrqState>) -> Self {
        Self { legacy, state }
    }
}

impl<L> HardIrqHandler for CqhciIrqHandle<L>
where
    L: SdMmcIrqHandle,
{
    fn ack(&mut self) -> IrqAck {
        let legacy = self.legacy.handle_irq();
        let cqhci = self
            .state
            .acknowledge(legacy.kind() == HostEventKind::Error);
        if !cqhci
            && matches!(
                legacy.kind(),
                HostEventKind::None | HostEventKind::CardInterrupt
            )
        {
            return IrqAck::spurious(0);
        }
        let control_bits = u64::from(!self.state.is_active());
        IrqAck::cleared(
            IrqQueueMask::from_queue(0),
            ControlEvent::new(0, control_bits),
        )
    }
}
