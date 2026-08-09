use alloc::collections::VecDeque;

use super::event::{Callback, IpiEvent};

#[derive(Default)]
pub(super) struct IpiEventQueue {
    events: VecDeque<IpiEvent>,
}

impl IpiEventQueue {
    pub(super) fn push(&mut self, source_cpu: usize, callback: Callback) {
        self.events.push_back(IpiEvent {
            source_cpu,
            callback,
        });
    }

    pub(super) fn pop_one(&mut self) -> Option<(usize, Callback)> {
        self.events
            .pop_front()
            .map(|event| (event.source_cpu, event.callback))
    }
}
