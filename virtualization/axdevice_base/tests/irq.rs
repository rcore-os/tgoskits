// Copyright 2025 The Axvisor Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

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

struct MockIrqSink {
    events: Mutex<Vec<IrqEvent>>,
    error: Option<IrqError>,
}

impl MockIrqSink {
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

impl WiredIrqSink for MockIrqSink {
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

#[test]
fn edge_line_pulses_sink() {
    let sink = Arc::new(MockIrqSink::new(None));
    let input = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    );
    let line = input.connect().unwrap();

    assert_eq!(line.pulse(), Ok(()));
    assert_eq!(
        sink.events(),
        vec![IrqEvent::Pulse(ControllerInputId::new(4))]
    );
}

#[test]
fn level_line_raises_and_lowers_sink() {
    let sink = Arc::new(MockIrqSink::new(None));
    let input = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    );
    let line = input.connect().unwrap();

    assert_eq!(line.raise(), Ok(()));
    assert_eq!(line.lower(), Ok(()));
    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::SetLevel(ControllerInputId::new(33), true),
            IrqEvent::SetLevel(ControllerInputId::new(33), false),
        ]
    );
}

#[test]
fn shared_level_input_stays_asserted_until_every_source_lowers() {
    let sink = Arc::new(MockIrqSink::new(None));
    let input = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    );
    let first_source = input.connect().unwrap();
    let second_source = input.connect().unwrap();

    first_source.raise().unwrap();
    second_source.raise().unwrap();
    first_source.lower().unwrap();

    assert_eq!(
        sink.events(),
        vec![IrqEvent::SetLevel(ControllerInputId::new(33), true)]
    );

    second_source.lower().unwrap();
    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::SetLevel(ControllerInputId::new(33), true),
            IrqEvent::SetLevel(ControllerInputId::new(33), false),
        ]
    );
}

#[test]
fn cloned_level_line_keeps_one_source_identity() {
    let sink = Arc::new(MockIrqSink::new(None));
    let input = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    );
    let line = input.connect().unwrap();
    let clone = line.clone();

    line.raise().unwrap();
    clone.raise().unwrap();
    clone.lower().unwrap();
    line.lower().unwrap();

    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::SetLevel(ControllerInputId::new(33), true),
            IrqEvent::SetLevel(ControllerInputId::new(33), false),
        ]
    );
}

#[test]
fn mismatched_line_operations_return_invalid_input() {
    let sink = Arc::new(MockIrqSink::new(None));
    let edge_input = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    );
    let level_input = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    );
    let edge_line = edge_input.connect().unwrap();
    let level_line = level_input.connect().unwrap();

    assert!(matches!(
        edge_line.raise(),
        Err(IrqError::InvalidTriggerMode {
            operation: "raise",
            ..
        })
    ));
    assert!(matches!(
        edge_line.lower(),
        Err(IrqError::InvalidTriggerMode {
            operation: "lower",
            ..
        })
    ));
    assert!(matches!(
        level_line.pulse(),
        Err(IrqError::InvalidTriggerMode {
            operation: "pulse",
            ..
        })
    ));
    assert!(sink.events().is_empty());
}

#[test]
fn sink_errors_are_propagated_without_committing_level_state() {
    let backend_error = IrqError::Backend {
        endpoint: InterruptEndpoint::Wired {
            controller: InterruptControllerId::new(0),
            input: ControllerInputId::new(4),
        },
        operation: "signal",
        detail: "controller unavailable".into(),
    };
    let sink = Arc::new(MockIrqSink::new(Some(backend_error.clone())));
    let edge_input = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    );
    let level_input = WiredIrqInput::new(
        InterruptControllerId::new(0),
        ControllerInputId::new(33),
        InterruptTriggerMode::LevelTriggered,
        sink,
    );
    let edge_line = edge_input.connect().unwrap();
    let level_line = level_input.connect().unwrap();

    assert_eq!(edge_line.pulse(), Err(backend_error.clone()));
    assert_eq!(level_line.raise(), Err(backend_error.clone()));
    assert_eq!(level_line.lower(), Ok(()));
    assert_eq!(level_line.raise(), Err(backend_error));
}
