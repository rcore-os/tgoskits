use alloc::boxed::Box;

extern crate alloc;

use rdif_block::{
    ControllerInitEndpoint, IdList, InitError, InitInput, InitIrqProgress, InitPoll, InitSchedule,
    InitialController, IrqHandler, IrqOutcome,
};

struct FakeInitialController {
    commands: usize,
    bound: bool,
    declared_sources: IdList,
    handler_available: bool,
}

impl InitialController for FakeInitialController {
    fn irq_sources(&self) -> IdList {
        self.declared_sources
    }

    fn take_irq_handler(&mut self, source_id: usize) -> Option<Box<dyn IrqHandler>> {
        assert_eq!(source_id, 0);
        if !self.handler_available {
            return None;
        }
        self.bound = true;
        Some(Box::new(FakeInitIrq))
    }

    fn service_deferred_irq(&mut self, source_id: usize) -> InitIrqProgress {
        assert_eq!(source_id, 0);
        InitIrqProgress::Acknowledged
    }

    fn poll_init(&mut self, input: InitInput) -> InitPoll<()> {
        assert!(
            self.bound,
            "the first command requires a bound IRQ endpoint"
        );
        self.commands += 1;
        if input.irq_sources.contains(0) {
            InitPoll::Ready(())
        } else {
            InitPoll::Pending(InitSchedule::new(false, IdList::from_bits(1), Some(1_000)).unwrap())
        }
    }
}

struct FakeInitIrq;

impl IrqHandler for FakeInitIrq {
    fn handle_irq(&mut self) -> IrqOutcome {
        IrqOutcome::handled_control()
    }
}

fn take_required_handlers(
    controller: &mut dyn InitialController,
) -> Result<Vec<Box<dyn IrqHandler>>, InitError> {
    let sources = controller.irq_sources();
    if sources.is_empty() {
        return Err(InitError::MissingInterrupt);
    }
    sources
        .iter()
        .map(|source_id| {
            controller
                .take_irq_handler(source_id)
                .ok_or(InitError::MissingInterrupt)
        })
        .collect()
}

fn drive_object_safe_endpoint(endpoint: &mut dyn InitialController) -> InitPoll<()> {
    endpoint.poll_init(InitInput::at(0))
}

#[test]
fn initial_controller_endpoint_is_object_safe_and_preserves_all_wake_causes() {
    let mut controller = FakeInitialController {
        commands: 0,
        bound: false,
        declared_sources: IdList::from_bits(1),
        handler_available: true,
    };
    let handlers = take_required_handlers(&mut controller)
        .expect("every declared initialization source must supply a handler");
    assert_eq!(handlers.len(), 1);

    let ControllerInitEndpoint::Pending(endpoint) =
        ControllerInitEndpoint::Pending(&mut controller)
    else {
        panic!("fake initialization must be pending");
    };
    let InitPoll::Pending(schedule) = drive_object_safe_endpoint(endpoint) else {
        panic!("the first command must await its IRQ or watchdog deadline");
    };

    assert!(!schedule.run_again());
    assert!(schedule.irq_sources().contains(0));
    assert_eq!(schedule.wake_at_ns(), Some(1_000));
    assert_eq!(controller.commands, 1);
}

#[test]
fn missing_initialization_irq_handler_fails_before_the_first_command() {
    let mut controller = FakeInitialController {
        commands: 0,
        bound: false,
        declared_sources: IdList::from_bits(1),
        handler_available: false,
    };

    assert!(matches!(
        take_required_handlers(&mut controller),
        Err(InitError::MissingInterrupt)
    ));
    assert_eq!(controller.commands, 0);
    assert!(!controller.bound);
}

#[test]
fn pending_initialization_without_an_irq_source_fails_closed() {
    let mut controller = FakeInitialController {
        commands: 0,
        bound: false,
        declared_sources: IdList::none(),
        handler_available: true,
    };

    assert!(matches!(
        take_required_handlers(&mut controller),
        Err(InitError::MissingInterrupt)
    ));
    assert_eq!(controller.commands, 0);
    assert!(!controller.bound);
}
