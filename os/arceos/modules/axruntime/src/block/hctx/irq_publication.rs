//! Hard-IRQ publication into one hardware queue's bounded event domain.

use core::sync::atomic::Ordering;

use rdif_block::AcknowledgedEvent;

use super::{HardwareQueue, HardwareQueueError};
use crate::block::{HctxCause, RingFull};

pub(super) const MAX_EVENTS: usize = 64;

#[derive(Clone, Copy)]
pub(super) struct EpochEvent {
    pub(super) epoch: u64,
    pub(super) event: AcknowledgedEvent,
}

impl HardwareQueue {
    /// Records one captured IRQ event from the fixed maintenance owner.
    pub(in crate::block) fn record_owner_irq_event(
        &self,
        expected_controller_epoch: u64,
        event: AcknowledgedEvent,
    ) -> Result<bool, HardwareQueueError> {
        let queue = self;
        if queue.fatal_completion_quarantine.load(Ordering::Acquire) {
            // The first unrepresentable completion closes this publisher before
            // controller recovery drains the IRQ action. Returning a typed
            // error makes the IRQ framework quench the current action rather
            // than silently acknowledging another snapshot that cannot be
            // serviced.
            assert!(
                queue.controller_link.request_irq_recovery(queue.info.id),
                "block IRQ recovery lost its shutdown-lifetime controller owner"
            );
            return Err(HardwareQueueError::Capacity);
        }
        let Some(epoch) = queue.control.accepted_event_epoch() else {
            return Ok(false);
        };
        if epoch != expected_controller_epoch {
            return Err(HardwareQueueError::StaleIrqEvent);
        }
        if event.for_queue(queue.info.id).is_none() {
            return Ok(false);
        }

        let publication = queue.terminal_gate.begin_irq_publication();
        let snapshot = EpochEvent { epoch, event };
        match queue.events.try_push(snapshot) {
            Ok(()) => {
                queue.control.raise(HctxCause::Irq);
                drop(publication);
                Ok(true)
            }
            Err(RingFull) => {
                // Publish recovery before asking the IRQ framework to quench
                // this action. The top half translates this typed error into
                // QuenchAndWake, so no further events can race the worker's
                // controller reset boundary.
                queue.control.raise(HctxCause::EventOverflow);
                drop(publication);
                Err(HardwareQueueError::EventOverflow {
                    queue_id: queue.info.id,
                })
            }
        }
    }
}
