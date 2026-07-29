use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rdif_block::{ControlEvent, HardIrqHandler, IrqDisposition};

use crate::os::{BlockIrqOutcome, BlockNotification};

/// Preallocated hard-IRQ action owning exactly one boxed device handler.
pub struct BlockIrqAction {
    handler: Box<dyn HardIrqHandler>,
    targets: Vec<IrqTarget>,
    controller_target: Option<ControllerIrqTarget>,
}

pub(super) struct IrqTarget {
    queue_id: usize,
    latch: Arc<IrqEventLatch>,
    notification: Arc<dyn BlockNotification>,
}

pub(super) struct IrqEventLatch {
    queue_ready: AtomicBool,
    needs_rearm: AtomicBool,
    control_bits: AtomicU64,
    source_id: usize,
}

pub(super) struct ControllerIrqTarget {
    latch: Arc<ControllerIrqLatch>,
    notification: Arc<dyn BlockNotification>,
}

pub(super) struct ControllerIrqLatch {
    needs_rearm: AtomicBool,
    control_bits: AtomicU64,
    source_id: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LatchedIrqEvent {
    pub(super) queue_ready: bool,
    pub(super) needs_rearm: bool,
    pub(super) control: ControlEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LatchedControllerIrq {
    pub(super) needs_rearm: bool,
    pub(super) control: ControlEvent,
}

impl BlockIrqAction {
    pub(super) fn new(handler: Box<dyn HardIrqHandler>, targets: Vec<IrqTarget>) -> Self {
        Self {
            handler,
            targets,
            controller_target: None,
        }
    }

    pub(super) fn with_controller_target(mut self, target: ControllerIrqTarget) -> Self {
        self.controller_target = Some(target);
        self
    }

    /// Runs the device-local acknowledgement and activates deferred workers.
    ///
    /// This is the complete hard IRQ path. It performs no allocation, queue
    /// drain, DMA copy, registry lookup, filesystem access, or business-task
    /// wakeup.
    pub fn run(&mut self) -> BlockIrqOutcome {
        let ack = self.handler.ack();
        if ack.is_spurious() {
            return BlockIrqOutcome::Unhandled;
        }

        let queues = ack.queues();
        let control = ack.control_event();
        let needs_rearm = matches!(ack.disposition(), IrqDisposition::MaskedNeedsRearm);
        let mut activated = false;
        let mut control_deferred = false;
        for target in &self.targets {
            if !queues.contains(target.queue_id) {
                continue;
            }
            // A controller transition may depend on queue-owned state produced
            // while draining this same acknowledged IRQ. Route its control
            // event through exactly one matching hctx so the queue observes
            // the hardware event before the controller state machine. Other
            // matching hctxs still receive their queue-ready and rearm state.
            let control_bits = if control_deferred { 0 } else { control.bits() };
            target.latch.publish(true, needs_rearm, control_bits);
            target.notification.notify_from_irq();
            activated = true;
            control_deferred |= control_bits != 0;
        }
        if !control_deferred
            && !control.is_empty()
            && let Some(target) = &self.controller_target
        {
            target.latch.publish(needs_rearm, control.bits());
            target.notification.notify_from_irq();
            activated = true;
        }
        if activated {
            BlockIrqOutcome::Wake
        } else {
            BlockIrqOutcome::Handled
        }
    }
}

impl ControllerIrqTarget {
    pub(super) fn new(
        latch: Arc<ControllerIrqLatch>,
        notification: Arc<dyn BlockNotification>,
    ) -> Self {
        Self {
            latch,
            notification,
        }
    }
}

impl ControllerIrqLatch {
    pub(super) const fn new(source_id: usize) -> Self {
        Self {
            needs_rearm: AtomicBool::new(false),
            control_bits: AtomicU64::new(0),
            source_id,
        }
    }

    fn publish(&self, needs_rearm: bool, control_bits: u64) {
        if needs_rearm {
            self.needs_rearm.store(true, Ordering::Release);
        }
        self.control_bits.fetch_or(control_bits, Ordering::AcqRel);
    }

    pub(super) fn take(&self) -> LatchedControllerIrq {
        LatchedControllerIrq {
            needs_rearm: self.needs_rearm.swap(false, Ordering::AcqRel),
            control: ControlEvent::new(self.source_id, self.control_bits.swap(0, Ordering::AcqRel)),
        }
    }
}

impl IrqTarget {
    pub(super) fn new(
        queue_id: usize,
        latch: Arc<IrqEventLatch>,
        notification: Arc<dyn BlockNotification>,
    ) -> Self {
        Self {
            queue_id,
            latch,
            notification,
        }
    }
}

impl IrqEventLatch {
    pub(super) const fn new(source_id: usize) -> Self {
        Self {
            queue_ready: AtomicBool::new(false),
            needs_rearm: AtomicBool::new(false),
            control_bits: AtomicU64::new(0),
            source_id,
        }
    }

    fn publish(&self, queue_ready: bool, needs_rearm: bool, control_bits: u64) {
        if queue_ready {
            self.queue_ready.store(true, Ordering::Release);
        }
        if needs_rearm {
            self.needs_rearm.store(true, Ordering::Release);
        }
        if control_bits != 0 {
            self.control_bits.fetch_or(control_bits, Ordering::AcqRel);
        }
    }

    pub(super) fn take(&self) -> LatchedIrqEvent {
        LatchedIrqEvent {
            queue_ready: self.queue_ready.swap(false, Ordering::AcqRel),
            needs_rearm: self.needs_rearm.swap(false, Ordering::AcqRel),
            control: ControlEvent::new(self.source_id, self.control_bits.swap(0, Ordering::AcqRel)),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use rdif_block::{ControlEvent, HardIrqHandler, IrqAck, IrqQueueMask};

    use super::*;

    struct TestNotification {
        irq_notifications: AtomicUsize,
    }

    impl BlockNotification for TestNotification {
        fn notify(&self) {}

        fn notify_from_irq(&self) {
            self.irq_notifications.fetch_add(1, Ordering::AcqRel);
        }

        fn wait(&self) {}

        fn wait_timeout(&self, _duration: core::time::Duration) -> bool {
            false
        }
    }

    struct FixedHandler {
        ack: IrqAck,
    }

    impl HardIrqHandler for FixedHandler {
        fn ack(&mut self) -> IrqAck {
            self.ack
        }
    }

    #[test]
    fn hard_irq_only_latches_and_notifies_deferred_work() {
        let latch = Arc::new(IrqEventLatch::new(5));
        let notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let target = IrqTarget::new(2, latch.clone(), notification.clone());
        let handler = FixedHandler {
            ack: IrqAck::cleared(IrqQueueMask::from_queue(2), ControlEvent::new(5, 0)),
        };
        let mut action = BlockIrqAction::new(Box::new(handler), vec![target]);

        assert_eq!(action.run(), BlockIrqOutcome::Wake);
        assert_eq!(notification.irq_notifications.load(Ordering::Acquire), 1);
        assert_eq!(
            latch.take(),
            LatchedIrqEvent {
                queue_ready: true,
                needs_rearm: false,
                control: ControlEvent::new(5, 0),
            }
        );
    }

    #[test]
    fn spurious_irq_does_not_activate_worker() {
        let latch = Arc::new(IrqEventLatch::new(7));
        let notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let target = IrqTarget::new(1, latch.clone(), notification.clone());
        let handler = FixedHandler {
            ack: IrqAck::spurious(7),
        };
        let mut action = BlockIrqAction::new(Box::new(handler), vec![target]);

        assert_eq!(action.run(), BlockIrqOutcome::Unhandled);
        assert_eq!(notification.irq_notifications.load(Ordering::Acquire), 0);
        assert!(!latch.take().queue_ready);
    }

    #[test]
    fn acknowledged_empty_irq_does_not_activate_worker() {
        let latch = Arc::new(IrqEventLatch::new(9));
        let notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let target = IrqTarget::new(3, latch.clone(), notification.clone());
        let handler = FixedHandler {
            ack: IrqAck::cleared(IrqQueueMask::none(), ControlEvent::new(9, 0)),
        };
        let mut action = BlockIrqAction::new(Box::new(handler), vec![target]);

        assert_eq!(action.run(), BlockIrqOutcome::Handled);
        assert_eq!(notification.irq_notifications.load(Ordering::Acquire), 0);
        assert!(!latch.take().queue_ready);
    }

    #[test]
    fn queue_coupled_control_is_deferred_to_hctx() {
        let queue_latch = Arc::new(IrqEventLatch::new(11));
        let queue_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let controller_latch = Arc::new(ControllerIrqLatch::new(11));
        let controller_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let handler = FixedHandler {
            ack: IrqAck::masked_needs_rearm(
                IrqQueueMask::from_queue(2),
                ControlEvent::new(11, 0x80),
            ),
        };
        let mut action = BlockIrqAction::new(
            Box::new(handler),
            vec![IrqTarget::new(
                2,
                queue_latch.clone(),
                queue_notification.clone(),
            )],
        )
        .with_controller_target(ControllerIrqTarget::new(
            controller_latch.clone(),
            controller_notification.clone(),
        ));

        assert_eq!(action.run(), BlockIrqOutcome::Wake);
        assert_eq!(
            queue_notification.irq_notifications.load(Ordering::Acquire),
            1
        );
        assert_eq!(
            controller_notification
                .irq_notifications
                .load(Ordering::Acquire),
            0
        );
        assert_eq!(
            queue_latch.take(),
            LatchedIrqEvent {
                queue_ready: true,
                needs_rearm: true,
                control: ControlEvent::new(11, 0x80),
            }
        );
        assert_eq!(
            controller_latch.take(),
            LatchedControllerIrq {
                needs_rearm: false,
                control: ControlEvent::new(11, 0),
            }
        );
    }
}
