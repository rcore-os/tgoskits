//! Hard-IRQ acknowledgement for a CQHCI engine sharing an SD/MMC IRQ line.

use alloc::sync::Arc;
use core::sync::atomic::{AtomicU64, Ordering};

use rdif_block::{ControlEvent, HardIrqHandler, IrqAck, IrqQueueMask};
use sdmmc_host::{HostEvent, HostEventKind, SdMmcIrqHandle};

use crate::{read_u32, regs::*, write_u32};

pub(crate) struct IrqState {
    base: usize,
    route: AtomicU64,
    mailbox: AtomicU64,
}

impl IrqState {
    pub(crate) fn new(base: usize) -> Self {
        Self {
            base,
            route: AtomicU64::new(0),
            mailbox: AtomicU64::new(0),
        }
    }

    pub(crate) fn set_active(&self, active: bool) -> (u32, u32) {
        // The task prevents new readers before waiting for the one IRQ owner.
        // The IRQ side never waits: a blocked observation leaves hardware
        // status latched. Route changes increment the generation only after
        // every old observation has published its mailbox snapshot.
        self.route.fetch_or(2, Ordering::AcqRel);
        let previous = loop {
            let route = self.route.load(Ordering::Acquire);
            if route & 4 == 0 {
                break route;
            }
            core::hint::spin_loop();
        };
        let retired = self.take();
        self.route.store(
            ((previous & !7).wrapping_add(8)) | u64::from(active),
            Ordering::Release,
        );
        retired
    }

    fn is_active(&self) -> bool {
        self.route.load(Ordering::Acquire) & 1 != 0
    }

    fn enter_irq(&self) -> Option<u64> {
        let route = self.route.load(Ordering::Acquire);
        if route & 6 != 0 {
            return None;
        }
        self.route
            .compare_exchange(route, route | 4, Ordering::AcqRel, Ordering::Acquire)
            .ok()
            .map(|_| route)
    }

    fn leave_irq(&self) {
        self.route.fetch_and(!4, Ordering::Release);
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

/// Releases the reader even if a platform IRQ implementation unwinds.
struct IrqReadGuard<'a>(&'a IrqState);
impl Drop for IrqReadGuard<'_> {
    fn drop(&mut self) {
        self.0.leave_irq();
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
        let Some(route) = self.state.enter_irq() else {
            return IrqAck::spurious(0);
        };
        let observation = IrqReadGuard(&self.state);
        let legacy = self.legacy.handle_irq();
        let cqhci = self
            .state
            .acknowledge(legacy.kind() == HostEventKind::Error);
        drop(observation);
        if !cqhci
            && matches!(
                legacy.kind(),
                HostEventKind::None | HostEventKind::CardInterrupt
            )
        {
            return IrqAck::spurious(0);
        }
        let control_bits = u64::from(route & 1 == 0);
        IrqAck::cleared(
            IrqQueueMask::from_queue(0),
            ControlEvent::new(0, control_bits),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn route_handoff_waits_for_old_irq_publication_and_retires_it() {
        let state = IrqState::new(0);
        state.set_active(true);
        let generation = state.enter_irq().unwrap();
        std::thread::scope(|scope| {
            let switching = scope.spawn(|| state.set_active(false));
            // Hold the previous observation across the task's handoff attempt.
            while state.route.load(Ordering::Acquire) & 2 == 0 {
                core::hint::spin_loop();
            }
            assert!(state.enter_irq().is_none());
            state.mailbox.store(u64::MAX, Ordering::Release);
            state.leave_irq();
            assert_eq!(switching.join().unwrap(), (u32::MAX, u32::MAX));
        });
        assert_eq!(state.take(), (0, 0));
        assert!(!state.is_active());
        let next_generation = state.enter_irq().unwrap();
        assert_ne!(generation & !7, next_generation & !7);
        state.leave_irq();
        state.set_active(true);
        assert_eq!(state.take(), (0, 0));
    }
}
