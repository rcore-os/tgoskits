use std::sync::{Arc, Mutex};

use axdevice_base::{
    ControllerInputId, InterruptControllerId, InterruptEndpoint, InterruptTriggerMode, IrqError,
    IrqResult, WiredIrqInput, WiredIrqSink,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IrqEvent {
    SetLevel(ControllerInputId, bool),
    Pulse(ControllerInputId),
}

struct RecordingSink {
    events: Mutex<Vec<IrqEvent>>,
    error: Option<IrqError>,
}

impl RecordingSink {
    fn new(error: Option<IrqError>) -> Self {
        Self {
            events: Mutex::new(Vec::new()),
            error,
        }
    }

    fn events(&self) -> Vec<IrqEvent> {
        self.events.lock().unwrap().clone()
    }
}

impl WiredIrqSink for RecordingSink {
    fn set_level(&self, input: ControllerInputId, asserted: bool) -> IrqResult {
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        self.events
            .lock()
            .unwrap()
            .push(IrqEvent::SetLevel(input, asserted));
        Ok(())
    }

    fn pulse(&self, input: ControllerInputId) -> IrqResult {
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        self.events.lock().unwrap().push(IrqEvent::Pulse(input));
        Ok(())
    }
}

fn input(
    input: ControllerInputId,
    trigger: InterruptTriggerMode,
    sink: Arc<RecordingSink>,
) -> WiredIrqInput {
    WiredIrqInput::new(InterruptControllerId::new(0), input, trigger, sink)
}

#[test]
fn edge_and_level_lines_deliver_their_trigger_semantics() {
    let sink = Arc::new(RecordingSink::new(None));
    let edge = input(
        ControllerInputId::new(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();
    let level = input(
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();

    edge.pulse().unwrap();
    level.assert().unwrap();
    level.deassert().unwrap();

    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::Pulse(ControllerInputId::new(4)),
            IrqEvent::SetLevel(ControllerInputId::new(33), true),
            IrqEvent::SetLevel(ControllerInputId::new(33), false),
        ]
    );
}

#[test]
fn trigger_mismatch_reports_the_typed_endpoint() {
    let sink = Arc::new(RecordingSink::new(None));
    let edge = WiredIrqInput::new(
        InterruptControllerId::new(7),
        ControllerInputId::new(9),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();
    let level = input(
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();

    assert!(matches!(
        edge.assert(),
        Err(IrqError::InvalidTriggerMode {
            endpoint: InterruptEndpoint::Wired {
                controller,
                input,
            },
            operation: "assert",
            ..
        }) if controller == InterruptControllerId::new(7)
            && input == ControllerInputId::new(9)
    ));
    assert!(matches!(
        edge.deassert(),
        Err(IrqError::InvalidTriggerMode {
            operation: "deassert",
            ..
        })
    ));
    assert!(matches!(
        level.pulse(),
        Err(IrqError::InvalidTriggerMode {
            operation: "pulse",
            ..
        })
    ));
    assert!(sink.events().is_empty());
}

#[test]
fn sink_errors_are_propagated_without_latching_failed_levels() {
    let endpoint = InterruptEndpoint::Wired {
        controller: InterruptControllerId::new(0),
        input: ControllerInputId::new(4),
    };
    let backend_error = IrqError::Backend {
        endpoint,
        operation: "signal",
        detail: "controller unavailable".into(),
    };
    let sink = Arc::new(RecordingSink::new(Some(backend_error.clone())));
    let edge = input(
        ControllerInputId::new(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    )
    .connect()
    .unwrap();
    let level = input(
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        sink,
    )
    .connect()
    .unwrap();

    assert_eq!(edge.pulse(), Err(backend_error.clone()));
    assert_eq!(level.assert(), Err(backend_error));
    assert_eq!(level.deassert(), Ok(()));
}

#[test]
fn shared_level_sources_use_wired_or_semantics() {
    let sink = Arc::new(RecordingSink::new(None));
    let input = WiredIrqInput::new(
        InterruptControllerId::new(2),
        ControllerInputId::new(41),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    );
    let first = input.connect().unwrap();
    let second = input.connect().unwrap();

    first.assert().unwrap();
    second.assert().unwrap();
    first.deassert().unwrap();
    assert_eq!(
        sink.events(),
        vec![IrqEvent::SetLevel(ControllerInputId::new(41), true)]
    );

    second.deassert().unwrap();
    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::SetLevel(ControllerInputId::new(41), true),
            IrqEvent::SetLevel(ControllerInputId::new(41), false),
        ]
    );
}

#[test]
fn dropping_an_asserted_source_releases_the_aggregate_level() {
    let sink = Arc::new(RecordingSink::new(None));
    let input = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    );

    let line = input.connect().unwrap();
    line.assert().unwrap();
    drop(line);

    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::SetLevel(ControllerInputId::new(33), true),
            IrqEvent::SetLevel(ControllerInputId::new(33), false),
        ]
    );
}
