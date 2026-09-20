use alloc::{boxed::Box, sync::Arc, vec::Vec};

use rdif_block::{
    ControlEvent, GroupIrqEvent, GroupIrqSink, GroupIrqTarget, HardIrqHandler, IrqDisposition,
    IrqQueueMask, SharedHardIrqHandler,
};

use crate::os::{BlockIrqOutcome, BlockNotification, sync::IrqMutex};

/// Preallocated hard-IRQ action owning exactly one boxed device handler.
pub struct BlockIrqAction {
    handler: BlockIrqHandler,
    targets: Vec<IrqTarget>,
    controller_target: Option<ControllerIrqTarget>,
    group_targets: Vec<GroupIrqMemberTarget>,
}

enum BlockIrqHandler {
    Device(Box<dyn HardIrqHandler>),
    Group(Box<dyn SharedHardIrqHandler>),
}

pub(super) struct GroupIrqMemberTarget {
    member_id: usize,
    targets: Vec<IrqTarget>,
    controller_target: Option<ControllerIrqTarget>,
}

pub(super) struct IrqTarget {
    queue_id: usize,
    latch: Arc<IrqEventLatch>,
    notification: Arc<dyn BlockNotification>,
}

pub(super) struct IrqEventLatch {
    pending: IrqMutex<LatchedIrqEvent>,
}

pub(super) struct ControllerIrqTarget {
    latch: Arc<ControllerIrqLatch>,
    notification: Arc<dyn BlockNotification>,
}

pub(super) struct ControllerIrqLatch {
    pending: IrqMutex<LatchedControllerIrq>,
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
            handler: BlockIrqHandler::Device(handler),
            targets,
            controller_target: None,
            group_targets: Vec::new(),
        }
    }

    pub(super) fn new_group(
        handler: Box<dyn SharedHardIrqHandler>,
        controller_target: Option<ControllerIrqTarget>,
        group_targets: Vec<GroupIrqMemberTarget>,
    ) -> Self {
        Self {
            handler: BlockIrqHandler::Group(handler),
            targets: Vec::new(),
            controller_target,
            group_targets,
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
        match &mut self.handler {
            BlockIrqHandler::Device(handler) => {
                run_device_irq(handler, &self.targets, self.controller_target.as_ref())
            }
            BlockIrqHandler::Group(handler) => {
                let mut sink = RuntimeGroupIrqSink {
                    controller_target: self.controller_target.as_ref(),
                    member_targets: &self.group_targets,
                    activated: false,
                    published: false,
                };
                let disposition = handler.ack(&mut sink);
                debug_assert!(
                    !matches!(disposition, IrqDisposition::Spurious) || !sink.published,
                    "a spurious shared IRQ must not publish events"
                );
                irq_outcome(disposition, sink.activated)
            }
        }
    }
}

impl GroupIrqMemberTarget {
    pub(super) fn new(
        member_id: usize,
        targets: Vec<IrqTarget>,
        controller_target: Option<ControllerIrqTarget>,
    ) -> Self {
        Self {
            member_id,
            targets,
            controller_target,
        }
    }
}

struct RuntimeGroupIrqSink<'a> {
    controller_target: Option<&'a ControllerIrqTarget>,
    member_targets: &'a [GroupIrqMemberTarget],
    activated: bool,
    published: bool,
}

impl GroupIrqSink for RuntimeGroupIrqSink<'_> {
    fn publish(&mut self, event: GroupIrqEvent) {
        self.published = true;
        self.activated |= match event.target() {
            GroupIrqTarget::Controller => publish_controller_event(self.controller_target, event),
            GroupIrqTarget::Member(member_id) => self
                .member_targets
                .iter()
                .find(|target| target.member_id == member_id)
                .is_some_and(|target| publish_member_event(target, event)),
        };
    }
}

fn run_device_irq(
    handler: &mut Box<dyn HardIrqHandler>,
    targets: &[IrqTarget],
    controller_target: Option<&ControllerIrqTarget>,
) -> BlockIrqOutcome {
    let ack = handler.ack();
    if ack.is_spurious() {
        return BlockIrqOutcome::Unhandled;
    }
    let activated = publish_device_event(
        targets,
        controller_target,
        ack.queues(),
        ack.control_event(),
        ack.disposition(),
    );
    irq_outcome(ack.disposition(), activated)
}

fn publish_member_event(target: &GroupIrqMemberTarget, event: GroupIrqEvent) -> bool {
    publish_device_event(
        &target.targets,
        target.controller_target.as_ref(),
        event.queues(),
        event.control(),
        event.disposition(),
    )
}

fn publish_controller_event(target: Option<&ControllerIrqTarget>, event: GroupIrqEvent) -> bool {
    let Some(target) = target else {
        return false;
    };
    let control = event.control();
    let needs_rearm = matches!(event.disposition(), IrqDisposition::MaskedNeedsRearm);
    if control.is_empty() && !needs_rearm {
        return false;
    }
    target.latch.publish(needs_rearm, control.bits());
    target.notification.notify();
    true
}

fn publish_device_event(
    targets: &[IrqTarget],
    controller_target: Option<&ControllerIrqTarget>,
    queues: IrqQueueMask,
    control: ControlEvent,
    disposition: IrqDisposition,
) -> bool {
    let needs_rearm = matches!(disposition, IrqDisposition::MaskedNeedsRearm);
    let mut activated = false;
    let mut control_deferred = false;
    for target in targets {
        if !queues.contains(target.queue_id) {
            continue;
        }
        // Queue state is published before controller rearm state so the task
        // drains the completion source before the controller observes it.
        let control_bits = if control_deferred { 0 } else { control.bits() };
        target.latch.publish(true, needs_rearm, control_bits);
        target.notification.notify();
        activated = true;
        control_deferred |= control_bits != 0;
    }
    // Supported masked domains (NVMe INTx and individual AHCI ports) each have
    // one queue target. A shared masked multi-queue domain would need a source
    // completion barrier before any target could rearm it.
    // A queue target owns the complete drain-then-rearm transaction. Publishing
    // the same rearm to the controller worker would let it unmask the source
    // before the hctx has consumed the completions. Controller-only events,
    // such as an admin IRQ without a queue target, still use this fallback.
    if !activated
        && (!control.is_empty() || needs_rearm)
        && let Some(target) = controller_target
    {
        target.latch.publish(needs_rearm, control.bits());
        target.notification.notify();
        activated = true;
    }
    activated
}

const fn irq_outcome(disposition: IrqDisposition, activated: bool) -> BlockIrqOutcome {
    if matches!(disposition, IrqDisposition::Spurious) {
        BlockIrqOutcome::Unhandled
    } else if activated {
        BlockIrqOutcome::Wake
    } else {
        BlockIrqOutcome::Handled
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
            pending: IrqMutex::new(LatchedControllerIrq {
                needs_rearm: false,
                control: ControlEvent::new(source_id, 0),
            }),
        }
    }

    fn publish(&self, needs_rearm: bool, control_bits: u64) {
        let mut pending = self.pending.lock();
        pending.needs_rearm |= needs_rearm;
        pending.control = ControlEvent::new(
            pending.control.source_id(),
            pending.control.bits() | control_bits,
        );
    }

    pub(super) fn take(&self) -> LatchedControllerIrq {
        let mut pending = self.pending.lock();
        let event = *pending;
        pending.needs_rearm = false;
        pending.control = ControlEvent::new(event.control.source_id(), 0);
        event
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
            pending: IrqMutex::new(LatchedIrqEvent {
                queue_ready: false,
                needs_rearm: false,
                control: ControlEvent::new(source_id, 0),
            }),
        }
    }

    fn publish(&self, queue_ready: bool, needs_rearm: bool, control_bits: u64) {
        // Drain, control and rearm belong to one event. Separate atomics let a
        // consumer take rearm from a new IRQ after taking an empty queue flag.
        let mut pending = self.pending.lock();
        pending.queue_ready |= queue_ready;
        pending.needs_rearm |= needs_rearm;
        pending.control = ControlEvent::new(
            pending.control.source_id(),
            pending.control.bits() | control_bits,
        );
    }

    pub(super) fn take(&self) -> LatchedIrqEvent {
        let event = {
            let mut pending = self.pending.lock();
            let event = *pending;
            pending.queue_ready = false;
            pending.needs_rearm = false;
            pending.control = ControlEvent::new(event.control.source_id(), 0);
            event
        };
        #[cfg(test)]
        tests::after_snapshot();
        event
    }
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, sync::Arc, vec};
    use core::sync::atomic::{AtomicUsize, Ordering};

    use rdif_block::{
        ControlEvent, GroupIrqEvent, GroupIrqSink, HardIrqHandler, IrqAck, IrqDisposition,
        IrqQueueMask, SharedHardIrqHandler,
    };

    use super::*;

    // Inject an IRQ at the consumer snapshot boundary without timing or threads.
    std::thread_local! {
        static AFTER_SNAPSHOT: core::cell::RefCell<Option<Box<dyn FnOnce()>>> =
            core::cell::RefCell::new(None);
    }

    pub(super) fn after_snapshot() {
        let callback = AFTER_SNAPSHOT.with(|slot| slot.borrow_mut().take());
        if let Some(callback) = callback {
            callback();
        }
    }

    #[test]
    fn irq_arriving_during_take_keeps_drain_and_rearm_together() {
        crate::os::task::install_test_runtime_ops();
        let latch = Arc::new(IrqEventLatch::new(11));
        let notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let mut action = BlockIrqAction::new(
            Box::new(FixedHandler {
                ack: IrqAck::masked_needs_rearm(
                    IrqQueueMask::from_queue(2),
                    ControlEvent::new(11, 0x80),
                ),
            }),
            vec![IrqTarget::new(2, latch.clone(), notification)],
        );
        AFTER_SNAPSHOT.with(|slot| {
            *slot.borrow_mut() = Some(Box::new(move || {
                assert_eq!(action.run(), BlockIrqOutcome::Wake);
            }));
        });

        // An IRQ after the snapshot belongs wholly to the next drain cycle.
        assert_eq!(
            latch.take(),
            LatchedIrqEvent {
                queue_ready: false,
                needs_rearm: false,
                control: ControlEvent::new(11, 0),
            },
        );
        assert_eq!(
            latch.take(),
            LatchedIrqEvent {
                queue_ready: true,
                needs_rearm: true,
                control: ControlEvent::new(11, 0x80),
            },
        );
        assert!(!latch.take().queue_ready);
    }

    struct TestNotification {
        irq_notifications: AtomicUsize,
    }

    impl BlockNotification for TestNotification {
        fn notify(&self) {
            assert!(!crate::os::sync::current_thread_holds_irq_mutex());
            self.irq_notifications.fetch_add(1, Ordering::AcqRel);
        }

        #[track_caller]
        fn wait(&self) {}

        #[track_caller]
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

    struct TwoMemberHandler {
        calls: Arc<AtomicUsize>,
    }

    impl SharedHardIrqHandler for TwoMemberHandler {
        fn ack(&mut self, sink: &mut dyn GroupIrqSink) -> IrqDisposition {
            self.calls.fetch_add(1, Ordering::AcqRel);
            sink.publish(GroupIrqEvent::member(
                2,
                IrqDisposition::Cleared,
                IrqQueueMask::from_queue(0),
                ControlEvent::new(0, 0x10),
            ));
            sink.publish(GroupIrqEvent::member(
                5,
                IrqDisposition::Cleared,
                IrqQueueMask::from_queue(0),
                ControlEvent::new(0, 0x20),
            ));
            IrqDisposition::Cleared
        }
    }

    #[test]
    fn hard_irq_only_latches_and_notifies_deferred_work() {
        crate::os::task::install_test_runtime_ops();
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
        crate::os::task::install_test_runtime_ops();
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
        crate::os::task::install_test_runtime_ops();
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
        crate::os::task::install_test_runtime_ops();
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

    #[test]
    fn queue_coupled_rearm_is_owned_only_by_hctx() {
        crate::os::task::install_test_runtime_ops();
        let queue_latch = Arc::new(IrqEventLatch::new(11));
        let queue_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let controller_latch = Arc::new(ControllerIrqLatch::new(11));
        let controller_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let handler = FixedHandler {
            ack: IrqAck::masked_needs_rearm(IrqQueueMask::from_queue(2), ControlEvent::new(11, 0)),
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
            0,
            "the controller must not race the hctx by rearming the same masked source"
        );
        assert_eq!(
            queue_latch.take(),
            LatchedIrqEvent {
                queue_ready: true,
                needs_rearm: true,
                control: ControlEvent::new(11, 0),
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

    #[test]
    fn one_shared_handler_fans_out_to_two_member_devices() {
        crate::os::task::install_test_runtime_ops();
        let first_latch = Arc::new(IrqEventLatch::new(0));
        let first_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let second_latch = Arc::new(IrqEventLatch::new(0));
        let second_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let calls = Arc::new(AtomicUsize::new(0));
        let handler = TwoMemberHandler {
            calls: Arc::clone(&calls),
        };
        let mut action = BlockIrqAction::new_group(
            Box::new(handler),
            None,
            vec![
                GroupIrqMemberTarget::new(
                    2,
                    vec![IrqTarget::new(
                        0,
                        Arc::clone(&first_latch),
                        first_notification.clone(),
                    )],
                    None,
                ),
                GroupIrqMemberTarget::new(
                    5,
                    vec![IrqTarget::new(
                        0,
                        Arc::clone(&second_latch),
                        second_notification.clone(),
                    )],
                    None,
                ),
            ],
        );

        assert_eq!(action.run(), BlockIrqOutcome::Wake);
        assert_eq!(calls.load(Ordering::Acquire), 1);
        assert_eq!(
            first_notification.irq_notifications.load(Ordering::Acquire),
            1
        );
        assert_eq!(
            second_notification
                .irq_notifications
                .load(Ordering::Acquire),
            1
        );
        assert_eq!(first_latch.take().control.bits(), 0x10);
        assert_eq!(second_latch.take().control.bits(), 0x20);
    }
}
