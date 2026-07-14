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

use axdevice_base::{InterruptTriggerMode, IrqError, IrqLine, IrqLineId, IrqResult, IrqSink};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum IrqEvent {
    SetLevel(IrqLineId, bool),
    Pulse(IrqLineId),
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

impl IrqSink for MockIrqSink {
    fn set_level(&self, line: IrqLineId, asserted: bool) -> IrqResult {
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        self.events
            .lock()
            .unwrap()
            .push(IrqEvent::SetLevel(line, asserted));
        Ok(())
    }

    fn pulse(&self, line: IrqLineId) -> IrqResult {
        if let Some(error) = self.error.clone() {
            return Err(error);
        }
        self.events.lock().unwrap().push(IrqEvent::Pulse(line));
        Ok(())
    }

    // eoi is intentionally absent — MockIrqSink only implements the upstream
    // IrqSink trait (set_level + pulse).
}

#[test]
fn edge_line_pulses_sink() {
    let sink = Arc::new(MockIrqSink::new(None));
    let line = IrqLine::new(
        IrqLineId(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    );

    assert_eq!(line.pulse(), Ok(()));
    assert_eq!(sink.events(), vec![IrqEvent::Pulse(IrqLineId(4))]);
}

#[test]
fn level_line_raises_and_lowers_sink() {
    let sink = Arc::new(MockIrqSink::new(None));
    let line = IrqLine::new(
        IrqLineId(33),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    );

    assert_eq!(line.raise(), Ok(()));
    assert_eq!(line.lower(), Ok(()));
    assert_eq!(
        sink.events(),
        vec![
            IrqEvent::SetLevel(IrqLineId(33), true),
            IrqEvent::SetLevel(IrqLineId(33), false),
        ]
    );
}

#[test]
fn mismatched_line_operations_return_invalid_input() {
    let sink = Arc::new(MockIrqSink::new(None));
    let edge_line = IrqLine::new(
        IrqLineId(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    );
    let level_line = IrqLine::new(
        IrqLineId(33),
        InterruptTriggerMode::LevelTriggered,
        sink.clone(),
    );

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
fn sink_errors_are_propagated() {
    let backend_error = IrqError::Backend {
        line: IrqLineId(4),
        operation: "signal",
        detail: "controller unavailable".into(),
    };
    let sink = Arc::new(MockIrqSink::new(Some(backend_error.clone())));
    let edge_line = IrqLine::new(
        IrqLineId(4),
        InterruptTriggerMode::EdgeTriggered,
        sink.clone(),
    );
    let level_line = IrqLine::new(IrqLineId(33), InterruptTriggerMode::LevelTriggered, sink);

    assert_eq!(edge_line.pulse(), Err(backend_error.clone()));
    assert_eq!(level_line.raise(), Err(backend_error.clone()));
    assert_eq!(level_line.lower(), Err(backend_error));
}
