use alloc::{boxed::Box, vec::Vec};

use crate::{
    BBlockController, BlkError, ControlEvent, ControllerState, IrqDisposition, IrqQueueMask,
};

/// Heap-owned lifecycle for one hardware controller that exposes several disks.
pub type BBlockControllerGroup = Box<dyn BlockControllerGroup>;

/// Stable target of an event produced by a shared hardware interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupIrqTarget {
    /// The hardware-owner group controller.
    Controller,
    /// One independently registered block device.
    Member(usize),
}

/// One preallocated event produced while acknowledging a shared interrupt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupIrqEvent {
    target: GroupIrqTarget,
    disposition: IrqDisposition,
    queues: IrqQueueMask,
    control: ControlEvent,
}

impl GroupIrqEvent {
    /// Creates an event for one member block device.
    pub const fn member(
        member_id: usize,
        disposition: IrqDisposition,
        queues: IrqQueueMask,
        control: ControlEvent,
    ) -> Self {
        Self {
            target: GroupIrqTarget::Member(member_id),
            disposition,
            queues,
            control,
        }
    }

    /// Creates a control-only event for the group lifecycle.
    pub const fn controller(disposition: IrqDisposition, control: ControlEvent) -> Self {
        Self {
            target: GroupIrqTarget::Controller,
            disposition,
            queues: IrqQueueMask::none(),
            control,
        }
    }

    /// Returns the group-local event target.
    pub const fn target(self) -> GroupIrqTarget {
        self.target
    }

    /// Returns how the driver handled this target's interrupt state.
    pub const fn disposition(self) -> IrqDisposition {
        self.disposition
    }

    /// Returns the member-local queues activated by this event.
    pub const fn queues(self) -> IrqQueueMask {
        self.queues
    }

    /// Returns driver-private control state associated with the event.
    pub const fn control(self) -> ControlEvent {
        self.control
    }
}

/// Receives events from one shared hard-IRQ handler without allocating.
pub trait GroupIrqSink {
    /// Publishes one controller- or member-local event.
    fn publish(&mut self, event: GroupIrqEvent);
}

/// Minimal hard-IRQ endpoint for a controller shared by several block devices.
pub trait SharedHardIrqHandler: Send + 'static {
    /// Acknowledges the physical source and publishes zero or more events.
    ///
    /// The handler must not allocate, drain a queue, complete DMA, or call an
    /// OS scheduler. It returns [`IrqDisposition::Spurious`] only when the
    /// physical controller did not assert the source.
    fn ack(&mut self, sink: &mut dyn GroupIrqSink) -> IrqDisposition;
}

/// Shared IRQ endpoint emitted by a controller-group transition.
pub struct SharedIrqEndpoint {
    source_id: usize,
    handler: Box<dyn SharedHardIrqHandler>,
}

impl SharedIrqEndpoint {
    /// Creates an endpoint whose handler is transferred to one IRQ token.
    pub fn new(source_id: usize, handler: Box<dyn SharedHardIrqHandler>) -> Self {
        Self { source_id, handler }
    }

    /// Returns the group-local physical IRQ source identifier.
    pub const fn source_id(&self) -> usize {
        self.source_id
    }

    /// Transfers the handler into the runtime IRQ registration.
    pub fn into_handler(self) -> Box<dyn SharedHardIrqHandler> {
        self.handler
    }
}

/// One block device produced by a hardware-controller group.
pub struct BlockGroupMember {
    member_id: usize,
    controller: BBlockController,
}

impl BlockGroupMember {
    /// Creates a member with a stable group-local identity.
    pub fn new(member_id: usize, controller: BBlockController) -> Self {
        Self {
            member_id,
            controller,
        }
    }

    /// Returns the stable group-local identity.
    pub const fn member_id(&self) -> usize {
        self.member_id
    }

    /// Transfers the member controller to the block runtime.
    pub fn into_controller(self) -> BBlockController {
        self.controller
    }

    /// Splits the member identity from its controller.
    pub fn into_parts(self) -> (usize, BBlockController) {
        (self.member_id, self.controller)
    }
}

/// Input that advances a [`BlockControllerGroup`] lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupControllerEvent {
    /// Starts the hardware owner and discovers member devices.
    Start,
    /// Retries a register-only transition.
    RegisterRetry,
    /// Delivers state acknowledged by the shared hard-IRQ handler.
    Irq(ControlEvent),
    /// Rearms shared device interrupt generation after targets are installed.
    Rearm { source_id: usize },
    /// Masks interrupt generation before the OS IRQ token is disabled.
    QuiesceIrqs,
    /// Stops every shared hardware resource after members are quiesced.
    Shutdown,
}

/// Resources and progress emitted by one group-controller transition.
pub struct GroupControllerUpdate {
    state: ControllerState,
    members: Vec<BlockGroupMember>,
    irq_endpoints: Vec<SharedIrqEndpoint>,
}

impl GroupControllerUpdate {
    /// Creates an update without newly transferred resources.
    pub const fn state(state: ControllerState) -> Self {
        Self {
            state,
            members: Vec::new(),
            irq_endpoints: Vec::new(),
        }
    }

    /// Creates an update that transfers discovered members and IRQ endpoints.
    pub fn with_resources(
        state: ControllerState,
        members: Vec<BlockGroupMember>,
        irq_endpoints: Vec<SharedIrqEndpoint>,
    ) -> Self {
        Self {
            state,
            members,
            irq_endpoints,
        }
    }

    /// Returns the group state after the transition.
    pub const fn controller_state(&self) -> ControllerState {
        self.state
    }

    /// Transfers newly discovered member devices to the runtime.
    pub fn take_members(&mut self) -> Vec<BlockGroupMember> {
        core::mem::take(&mut self.members)
    }

    /// Transfers shared IRQ endpoints to runtime registration tokens.
    pub fn take_irq_endpoints(&mut self) -> Vec<SharedIrqEndpoint> {
        core::mem::take(&mut self.irq_endpoints)
    }
}

/// Portable lifecycle for one hardware owner that exposes multiple disks.
pub trait BlockControllerGroup: crate::DriverGeneric {
    /// Advances shared initialization, IRQ rearm, quiesce, or shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error when the host transition fails. Member-local
    /// initialization failures should be represented by the member controller
    /// so healthy siblings can still start.
    fn advance(&mut self, event: GroupControllerEvent) -> Result<GroupControllerUpdate, BlkError>;
}

#[cfg(test)]
mod tests {
    use alloc::{boxed::Box, vec::Vec};

    use super::*;
    use crate::{BlockController, ControllerEvent, ControllerUpdate, DeviceInfo, IrqQueueMask};

    struct NoopController;

    impl crate::DriverGeneric for NoopController {
        fn name(&self) -> &str {
            "member"
        }
    }

    impl BlockController for NoopController {
        fn device_info(&self) -> DeviceInfo {
            DeviceInfo::new(8, 512)
        }

        fn max_io_queues(&self) -> usize {
            1
        }

        fn advance(&mut self, _event: ControllerEvent) -> Result<ControllerUpdate, BlkError> {
            Ok(ControllerUpdate::state(ControllerState::Ready))
        }
    }

    struct EventHandler;

    impl SharedHardIrqHandler for EventHandler {
        fn ack(&mut self, sink: &mut dyn GroupIrqSink) -> IrqDisposition {
            sink.publish(GroupIrqEvent::member(
                3,
                IrqDisposition::Cleared,
                IrqQueueMask::from_queue(0),
                ControlEvent::new(5, 0x20),
            ));
            IrqDisposition::Cleared
        }
    }

    #[derive(Default)]
    struct Events(Vec<GroupIrqEvent>);

    impl GroupIrqSink for Events {
        fn publish(&mut self, event: GroupIrqEvent) {
            self.0.push(event);
        }
    }

    #[test]
    fn member_and_shared_irq_ownership_are_move_only() {
        let member = BlockGroupMember::new(3, Box::new(NoopController));
        let endpoint = SharedIrqEndpoint::new(5, Box::new(EventHandler));
        let mut update = GroupControllerUpdate::with_resources(
            ControllerState::Ready,
            alloc::vec![member],
            alloc::vec![endpoint],
        );

        let member = update.take_members().remove(0);
        assert_eq!(member.member_id(), 3);
        assert_eq!(member.into_controller().name(), "member");

        let mut handler = update.take_irq_endpoints().remove(0).into_handler();
        let mut events = Events::default();
        assert_eq!(handler.ack(&mut events), IrqDisposition::Cleared);
        assert_eq!(
            events.0,
            alloc::vec![GroupIrqEvent::member(
                3,
                IrqDisposition::Cleared,
                IrqQueueMask::from_queue(0),
                ControlEvent::new(5, 0x20),
            )]
        );
    }
}
