use core::sync::atomic::{AtomicUsize, Ordering};

use rdif_block::IrqDisposition;

use super::ControllerIrqTarget;

const IN_PROGRESS: usize = 1 << (usize::BITS - 1);
const REARM_PENDING: usize = 1 << (usize::BITS - 2);
const REARM_CANCELLED: usize = 1 << (usize::BITS - 3);
const ACTIVE_TARGET_MASK: usize = REARM_CANCELLED - 1;

/// Rearm-domain equivalent of Linux's IRQ in-progress and oneshot state.
///
/// The low bits count deferred queue owners. The three high bits serialize the
/// hard-IRQ publication phase, the single rearm obligation, and terminal rearm
/// cancellation. Keeping all facts in one atomic word makes the last-owner
/// transition race-free: a new hard IRQ, a newly activated queue target, a
/// failed drain, or the final queue worker changes the same value that a rearm
/// claimant compares.
///
/// A rearm domain usually is one physical IRQ source. A shared controller may
/// instead expose independently masked member-local domains, such as AHCI
/// ports: those domains deliberately share a delivery `source_id` while each
/// retains its own episode and controller publisher.
pub(crate) struct IrqRearmEpisode {
    source_id: usize,
    state: AtomicUsize,
    controller: ControllerIrqTarget,
}

impl IrqRearmEpisode {
    pub(crate) const fn new(source_id: usize, controller: ControllerIrqTarget) -> Self {
        Self {
            source_id,
            state: AtomicUsize::new(0),
            controller,
        }
    }

    pub(crate) const fn source_id(&self) -> usize {
        self.source_id
    }

    /// Marks the non-reentrant hard-IRQ publication interval.
    pub(crate) fn begin_irq(&self) {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & IN_PROGRESS == 0).then_some(state | IN_PROGRESS)
            })
            .expect("one block IRQ source action must not execute reentrantly");
    }

    /// Accounts one queue owner before its IRQ notification is published.
    pub(crate) fn activate_target(&self) {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & ACTIVE_TARGET_MASK != ACTIVE_TARGET_MASK).then_some(state + 1)
            })
            .expect("block IRQ source has too many active deferred owners");
    }

    /// Ends hard-IRQ publication and claims an immediately ready rearm.
    ///
    /// `true` means the hard-IRQ caller is the unique rearm publisher. This is
    /// possible only for a control-only source with no deferred queue owner.
    pub(crate) fn finish_irq(&self, disposition: IrqDisposition) -> bool {
        let needs_rearm = matches!(disposition, IrqDisposition::MaskedNeedsRearm);
        let mut observed = self.state.load(Ordering::Acquire);
        loop {
            assert_ne!(
                observed & IN_PROGRESS,
                0,
                "block IRQ source publication ended without begin_irq"
            );
            let mut next = observed & !IN_PROGRESS;
            if needs_rearm && next & REARM_CANCELLED == 0 {
                next |= REARM_PENDING;
            }
            match self.state.compare_exchange_weak(
                observed,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(current) => observed = current,
            }
        }
        self.try_claim_rearm()
    }

    /// Releases one queue owner and claims the source rearm when it was last.
    pub(crate) fn finish_target(&self) -> bool {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                (state & ACTIVE_TARGET_MASK != 0).then_some(state - 1)
            })
            .expect("block IRQ target completed without active source ownership");
        self.try_claim_rearm()
    }

    /// Cancels the rearm obligation after a terminal queue failure.
    pub(crate) fn cancel_rearm(&self) {
        self.state
            .try_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                Some((state | REARM_CANCELLED) & !REARM_PENDING)
            })
            .expect("block IRQ rearm cancellation always has a next state");
    }

    pub(crate) fn publish_from_irq(&self, needs_rearm: bool, control_bits: u64) {
        self.controller.publish_from_irq(needs_rearm, control_bits);
    }

    pub(crate) fn publish_from_task(&self, needs_rearm: bool, control_bits: u64) {
        self.controller.publish_from_task(needs_rearm, control_bits);
    }

    fn try_claim_rearm(&self) -> bool {
        self.state
            .compare_exchange(REARM_PENDING, 0, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    #[cfg(test)]
    pub(crate) fn active_targets(&self) -> usize {
        self.state.load(Ordering::Acquire) & ACTIVE_TARGET_MASK
    }
}
