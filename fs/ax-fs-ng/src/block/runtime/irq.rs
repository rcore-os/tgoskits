mod source;

use alloc::{boxed::Box, sync::Arc, vec::Vec};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};

use rdif_block::{
    ControlEvent, GroupIrqEvent, GroupIrqSink, GroupIrqTarget, HardIrqHandler, IrqDisposition,
    IrqQueueMask, SharedHardIrqHandler,
};
pub(super) use source::IrqRearmEpisode;

use crate::os::{BlockIrqOutcome, BlockNotification};

const TARGET_ACTIVE: u8 = 1 << 0;
const TARGET_PENDING: u8 = 1 << 1;

/// Preallocated hard-IRQ action owning exactly one boxed device handler.
pub struct BlockIrqAction {
    handler: BlockIrqHandler,
    source: Option<Arc<IrqRearmEpisode>>,
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
    source: Arc<IrqRearmEpisode>,
    targets: Vec<IrqTarget>,
}

pub(super) struct IrqTarget {
    queue_id: usize,
    latch: Arc<IrqEventLatch>,
    notification: Arc<dyn BlockNotification>,
}

pub(super) struct IrqEventLatch {
    queue_ready: AtomicBool,
    control_bits: AtomicU64,
    target_state: AtomicU8,
    source: Arc<IrqRearmEpisode>,
}

#[derive(Clone)]
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
    pub(super) control: ControlEvent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct LatchedControllerIrq {
    pub(super) needs_rearm: bool,
    pub(super) control: ControlEvent,
}

impl BlockIrqAction {
    pub(super) fn new(
        handler: Box<dyn HardIrqHandler>,
        source: Arc<IrqRearmEpisode>,
        targets: Vec<IrqTarget>,
    ) -> Self {
        Self {
            handler: BlockIrqHandler::Device(handler),
            source: Some(source),
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
            source: None,
            targets: Vec::new(),
            controller_target,
            group_targets,
        }
    }

    /// Runs the device-local acknowledgement and activates deferred workers.
    ///
    /// This is the complete hard IRQ path. It performs no allocation, queue
    /// drain, DMA copy, registry lookup, filesystem access, or business-task
    /// wakeup.
    pub fn run(&mut self) -> BlockIrqOutcome {
        match &mut self.handler {
            BlockIrqHandler::Device(handler) => run_device_irq(
                handler,
                self.source
                    .as_ref()
                    .expect("device IRQ action owns its source episode"),
                &self.targets,
            ),
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
                debug_assert!(
                    !matches!(disposition, IrqDisposition::MaskedNeedsRearm) || sink.activated,
                    "a masked shared IRQ source must publish its deferred rearm owner"
                );
                irq_outcome(disposition, sink.activated)
            }
        }
    }
}

impl GroupIrqMemberTarget {
    pub(super) fn new(
        member_id: usize,
        source: Arc<IrqRearmEpisode>,
        targets: Vec<IrqTarget>,
    ) -> Self {
        Self {
            member_id,
            source,
            targets,
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
    source: &IrqRearmEpisode,
    targets: &[IrqTarget],
) -> BlockIrqOutcome {
    source.begin_irq();
    let ack = handler.ack();
    if ack.is_spurious() {
        let rearm_ready = source.finish_irq(ack.disposition());
        debug_assert!(!rearm_ready, "a spurious IRQ cannot request rearm");
        return BlockIrqOutcome::Unhandled;
    }
    let activated = publish_device_event(source, targets, ack.queues(), ack.control_event());
    let rearm_activated = if source.finish_irq(ack.disposition()) {
        source.publish_from_irq(true, 0);
        true
    } else {
        false
    };
    irq_outcome(ack.disposition(), activated || rearm_activated)
}

fn publish_member_event(target: &GroupIrqMemberTarget, event: GroupIrqEvent) -> bool {
    target.source.begin_irq();
    let activated = publish_device_event(
        &target.source,
        &target.targets,
        event.queues(),
        event.control(),
    );
    let rearm_activated = if target.source.finish_irq(event.disposition()) {
        target.source.publish_from_irq(true, 0);
        true
    } else {
        false
    };
    activated || rearm_activated
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
    target.publish_from_irq(needs_rearm, control.bits());
    true
}

fn publish_device_event(
    source: &IrqRearmEpisode,
    targets: &[IrqTarget],
    queues: IrqQueueMask,
    control: ControlEvent,
) -> bool {
    let mut activated = false;
    let mut control_deferred = false;
    for target in targets {
        if !queues.contains(target.queue_id) {
            continue;
        }
        let control_bits = if control_deferred { 0 } else { control.bits() };
        target.latch.publish(true, control_bits);
        target.notification.notify_from_irq();
        activated = true;
        control_deferred |= control_bits != 0;
    }
    if !control_deferred && !control.is_empty() {
        // A control-only source has no queue owner. Queue-coupled control is
        // published by the hctx after it has drained the acknowledged queue.
        source.publish_from_irq(false, control.bits());
        activated = true;
    }
    debug_assert_eq!(
        source.source_id(),
        control.source_id(),
        "one endpoint must publish only its fixed IRQ source"
    );
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

    fn publish_from_irq(&self, needs_rearm: bool, control_bits: u64) {
        self.latch.publish(needs_rearm, control_bits);
        self.notification.notify_from_irq();
    }

    pub(super) fn publish_from_task(&self, needs_rearm: bool, control_bits: u64) {
        self.latch.publish(needs_rearm, control_bits);
        self.notification.notify();
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

    pub(super) const fn source_id(&self) -> usize {
        self.source_id
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
    pub(super) fn new(source: Arc<IrqRearmEpisode>) -> Self {
        Self {
            queue_ready: AtomicBool::new(false),
            control_bits: AtomicU64::new(0),
            target_state: AtomicU8::new(0),
            source,
        }
    }

    fn publish(&self, queue_ready: bool, control_bits: u64) {
        if queue_ready {
            self.queue_ready.store(true, Ordering::Release);
        }
        if control_bits != 0 {
            self.control_bits.fetch_or(control_bits, Ordering::AcqRel);
        }
        let previous = self
            .target_state
            .fetch_or(TARGET_ACTIVE | TARGET_PENDING, Ordering::AcqRel);
        if previous & TARGET_ACTIVE == 0 {
            self.source.activate_target();
        }
    }

    pub(super) fn claim(&self) -> Option<LatchedIrqEvent> {
        let previous = self
            .target_state
            .fetch_and(!TARGET_PENDING, Ordering::AcqRel);
        if previous & TARGET_PENDING == 0 {
            return None;
        }
        Some(LatchedIrqEvent {
            queue_ready: self.queue_ready.swap(false, Ordering::AcqRel),
            control: ControlEvent::new(
                self.source.source_id(),
                self.control_bits.swap(0, Ordering::AcqRel),
            ),
        })
    }

    /// Completes one Linux `RUNTHREAD`-style deferred execution.
    ///
    /// A concurrent publisher sets `TARGET_PENDING` in the same atomic byte,
    /// so it either defeats this compare-exchange or observes an inactive
    /// target and re-acquires source ownership before notifying the worker.
    pub(super) fn finish(&self, allow_rearm: bool) -> bool {
        loop {
            let observed = self.target_state.load(Ordering::Acquire);
            assert_ne!(
                observed & TARGET_ACTIVE,
                0,
                "block IRQ target finished without active ownership"
            );
            if observed & TARGET_PENDING != 0 {
                return false;
            }
            if self
                .target_state
                .compare_exchange_weak(observed, 0, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                if !allow_rearm {
                    self.source.cancel_rearm();
                }
                return self.source.finish_target();
            }
        }
    }

    pub(super) fn finish_and_publish(&self, control: ControlEvent, allow_rearm: bool) {
        if !control.is_empty() {
            self.source.publish_from_task(false, control.bits());
        }
        if self.finish(allow_rearm) {
            self.source.publish_from_task(true, 0);
        }
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

    struct TestNotification {
        irq_notifications: AtomicUsize,
    }

    impl BlockNotification for TestNotification {
        fn notify(&self) {
            self.irq_notifications.fetch_add(1, Ordering::AcqRel);
        }

        fn notify_from_irq(&self) {
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
                IrqDisposition::MaskedNeedsRearm,
                IrqQueueMask::from_queue(0),
                ControlEvent::new(0, 0x10),
            ));
            sink.publish(GroupIrqEvent::member(
                5,
                IrqDisposition::MaskedNeedsRearm,
                IrqQueueMask::from_queue(0),
                ControlEvent::new(0, 0x20),
            ));
            IrqDisposition::Cleared
        }
    }

    fn test_source(
        source_id: usize,
    ) -> (
        Arc<IrqRearmEpisode>,
        Arc<ControllerIrqLatch>,
        Arc<TestNotification>,
    ) {
        let controller_latch = Arc::new(ControllerIrqLatch::new(source_id));
        let controller_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let controller_target = ControllerIrqTarget::new(
            Arc::clone(&controller_latch),
            controller_notification.clone(),
        );
        (
            Arc::new(IrqRearmEpisode::new(source_id, controller_target)),
            controller_latch,
            controller_notification,
        )
    }

    fn finish_target(latch: &IrqEventLatch, source_id: usize) {
        latch.finish_and_publish(ControlEvent::new(source_id, 0), true);
        debug_assert_eq!(latch.source.source_id(), source_id);
    }

    #[test]
    fn hard_irq_only_latches_and_notifies_deferred_work() {
        let (source, ..) = test_source(5);
        let latch = Arc::new(IrqEventLatch::new(Arc::clone(&source)));
        let notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let target = IrqTarget::new(2, latch.clone(), notification.clone());
        let handler = FixedHandler {
            ack: IrqAck::cleared(IrqQueueMask::from_queue(2), ControlEvent::new(5, 0)),
        };
        let mut action = BlockIrqAction::new(Box::new(handler), source, vec![target]);

        assert_eq!(action.run(), BlockIrqOutcome::Wake);
        assert_eq!(notification.irq_notifications.load(Ordering::Acquire), 1);
        assert_eq!(
            latch.claim(),
            Some(LatchedIrqEvent {
                queue_ready: true,
                control: ControlEvent::new(5, 0),
            })
        );
    }

    #[test]
    fn spurious_irq_does_not_activate_worker() {
        let (source, ..) = test_source(7);
        let latch = Arc::new(IrqEventLatch::new(Arc::clone(&source)));
        let notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let target = IrqTarget::new(1, latch.clone(), notification.clone());
        let handler = FixedHandler {
            ack: IrqAck::spurious(7),
        };
        let mut action = BlockIrqAction::new(Box::new(handler), source, vec![target]);

        assert_eq!(action.run(), BlockIrqOutcome::Unhandled);
        assert_eq!(notification.irq_notifications.load(Ordering::Acquire), 0);
        assert_eq!(latch.claim(), None);
    }

    #[test]
    fn acknowledged_empty_irq_does_not_activate_worker() {
        let (source, ..) = test_source(9);
        let latch = Arc::new(IrqEventLatch::new(Arc::clone(&source)));
        let notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let target = IrqTarget::new(3, latch.clone(), notification.clone());
        let handler = FixedHandler {
            ack: IrqAck::cleared(IrqQueueMask::none(), ControlEvent::new(9, 0)),
        };
        let mut action = BlockIrqAction::new(Box::new(handler), source, vec![target]);

        assert_eq!(action.run(), BlockIrqOutcome::Handled);
        assert_eq!(notification.irq_notifications.load(Ordering::Acquire), 0);
        assert_eq!(latch.claim(), None);
    }

    #[test]
    fn queue_coupled_control_is_deferred_to_hctx() {
        let (source, controller_latch, controller_notification) = test_source(11);
        let queue_latch = Arc::new(IrqEventLatch::new(Arc::clone(&source)));
        let queue_notification = Arc::new(TestNotification {
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
            source,
            vec![IrqTarget::new(
                2,
                queue_latch.clone(),
                queue_notification.clone(),
            )],
        );

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
            queue_latch.claim(),
            Some(LatchedIrqEvent {
                queue_ready: true,
                control: ControlEvent::new(11, 0x80),
            })
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
    fn queue_coupled_rearm_without_control_stays_with_hctx() {
        let (source, controller_latch, controller_notification) = test_source(11);
        let queue_latch = Arc::new(IrqEventLatch::new(Arc::clone(&source)));
        let queue_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let handler = FixedHandler {
            ack: IrqAck::masked_needs_rearm(IrqQueueMask::from_queue(2), ControlEvent::new(11, 0)),
        };
        let mut action = BlockIrqAction::new(
            Box::new(handler),
            source,
            vec![IrqTarget::new(
                2,
                queue_latch.clone(),
                queue_notification.clone(),
            )],
        );

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
            "the source must stay masked until its queue owner drains completions"
        );
        assert_eq!(
            queue_latch.claim(),
            Some(LatchedIrqEvent {
                queue_ready: true,
                control: ControlEvent::new(11, 0),
            })
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
    fn one_source_rearms_only_after_every_queue_owner_finishes() {
        let (source, controller_latch, controller_notification) = test_source(13);
        let first_latch = Arc::new(IrqEventLatch::new(Arc::clone(&source)));
        let second_latch = Arc::new(IrqEventLatch::new(Arc::clone(&source)));
        let first_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let second_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let handler = FixedHandler {
            ack: IrqAck::masked_needs_rearm(
                IrqQueueMask::from_bits((1 << 1) | (1 << 4)),
                ControlEvent::new(13, 0),
            ),
        };
        let mut action = BlockIrqAction::new(
            Box::new(handler),
            Arc::clone(&source),
            vec![
                IrqTarget::new(1, Arc::clone(&first_latch), first_notification),
                IrqTarget::new(4, Arc::clone(&second_latch), second_notification),
            ],
        );

        assert_eq!(action.run(), BlockIrqOutcome::Wake);
        assert_eq!(source.active_targets(), 2);
        assert!(first_latch.claim().is_some());
        assert!(second_latch.claim().is_some());

        finish_target(&first_latch, 13);
        assert_eq!(source.active_targets(), 1);
        assert_eq!(
            controller_notification
                .irq_notifications
                .load(Ordering::Acquire),
            0,
            "the first queue owner must not unmask a shared source"
        );

        finish_target(&second_latch, 13);
        assert_eq!(source.active_targets(), 0);
        assert_eq!(
            controller_notification
                .irq_notifications
                .load(Ordering::Acquire),
            1
        );
        assert!(controller_latch.take().needs_rearm);
    }

    #[test]
    fn irq_published_during_drain_keeps_runthread_active() {
        let (source, controller_latch, controller_notification) = test_source(17);
        let queue_latch = Arc::new(IrqEventLatch::new(Arc::clone(&source)));
        let queue_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let handler = FixedHandler {
            ack: IrqAck::masked_needs_rearm(IrqQueueMask::from_queue(2), ControlEvent::new(17, 0)),
        };
        let mut action = BlockIrqAction::new(
            Box::new(handler),
            Arc::clone(&source),
            vec![IrqTarget::new(
                2,
                Arc::clone(&queue_latch),
                queue_notification,
            )],
        );

        assert_eq!(action.run(), BlockIrqOutcome::Wake);
        assert!(queue_latch.claim().is_some());
        assert_eq!(action.run(), BlockIrqOutcome::Wake);

        finish_target(&queue_latch, 17);
        assert_eq!(source.active_targets(), 1);
        assert_eq!(
            controller_notification
                .irq_notifications
                .load(Ordering::Acquire),
            0
        );
        assert!(queue_latch.claim().is_some());
        finish_target(&queue_latch, 17);

        assert_eq!(source.active_targets(), 0);
        assert_eq!(
            controller_notification
                .irq_notifications
                .load(Ordering::Acquire),
            1
        );
        assert!(controller_latch.take().needs_rearm);
    }

    #[test]
    fn failed_drain_before_hard_irq_exit_cancels_rearm() {
        let (source, controller_latch, controller_notification) = test_source(19);
        let queue_latch = IrqEventLatch::new(Arc::clone(&source));

        source.begin_irq();
        queue_latch.publish(true, 0);
        assert!(queue_latch.claim().is_some());
        assert!(!queue_latch.finish(false));
        assert!(!source.finish_irq(IrqDisposition::MaskedNeedsRearm));

        assert_eq!(
            controller_notification
                .irq_notifications
                .load(Ordering::Acquire),
            0
        );
        assert!(!controller_latch.take().needs_rearm);
    }

    #[test]
    fn one_shared_handler_keeps_member_local_rearm_domains_independent() {
        let (first_source, first_controller, first_controller_notification) = test_source(0);
        let first_latch = Arc::new(IrqEventLatch::new(Arc::clone(&first_source)));
        let first_notification = Arc::new(TestNotification {
            irq_notifications: AtomicUsize::new(0),
        });
        let (second_source, second_controller, second_controller_notification) = test_source(0);
        let second_latch = Arc::new(IrqEventLatch::new(Arc::clone(&second_source)));
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
                    first_source,
                    vec![IrqTarget::new(
                        0,
                        Arc::clone(&first_latch),
                        first_notification.clone(),
                    )],
                ),
                GroupIrqMemberTarget::new(
                    5,
                    second_source,
                    vec![IrqTarget::new(
                        0,
                        Arc::clone(&second_latch),
                        second_notification.clone(),
                    )],
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
        assert_eq!(first_latch.claim().unwrap().control.bits(), 0x10);
        assert_eq!(second_latch.claim().unwrap().control.bits(), 0x20);

        finish_target(&first_latch, 0);
        assert!(first_controller.take().needs_rearm);
        assert_eq!(
            first_controller_notification
                .irq_notifications
                .load(Ordering::Acquire),
            1
        );
        assert!(!second_controller.take().needs_rearm);
        assert_eq!(
            second_controller_notification
                .irq_notifications
                .load(Ordering::Acquire),
            0,
            "finishing one AHCI port must not rearm another port's PxIE domain"
        );

        finish_target(&second_latch, 0);
        assert!(second_controller.take().needs_rearm);
        assert_eq!(
            second_controller_notification
                .irq_notifications
                .load(Ordering::Acquire),
            1
        );
    }
}
