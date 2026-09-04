use alloc::sync::Arc;

use axdevice_base::{DeviceError, DeviceResult};

use super::{
    ActivityPermit, InterruptPublicationKind, InterruptPublicationRequest, QueueNotification,
    QueueNotifyOutcome, VirtioDeviceCore, VirtioPciTransport, VirtioPciWriteOutcome,
    VirtioQueueGeneration, invalid_queue, map_pci_error,
};
use crate::{
    GuestMemory, NoGuestMemoryAccessor, VirtioQueue,
    constants::{
        VIRTIO_STATUS_DEVICE_NEEDS_RESET, VIRTIO_STATUS_DRIVER_OK, VIRTIO_STATUS_FAILED,
        VIRTIO_STATUS_FEATURES_OK,
    },
};

impl<D: VirtioDeviceCore> VirtioPciTransport<D> {
    pub(super) fn notify_queue(
        &self,
        queue_index: u16,
        dma_enabled: bool,
        memory: &mut dyn GuestMemory,
    ) -> DeviceResult<VirtioPciWriteOutcome> {
        let selected = {
            let state = self.state.lock();
            if queue_index as usize >= state.queues.len() {
                return Err(invalid_queue(queue_index));
            }
            if !queue_processing_enabled(state.status) {
                return Ok(self.idle_notification());
            }
            if !state.queues[queue_index as usize].enabled {
                return Ok(self.idle_notification());
            }
            if state.queues[queue_index as usize].processing {
                return Ok(self.idle_notification());
            }
            if !dma_enabled {
                return Ok(self.idle_notification());
            }
            (
                queue_index as usize,
                VirtioQueueGeneration(state.queue_generation),
            )
        };

        let (selected, generation) = selected;
        #[cfg(test)]
        self.run_notify_admission_hook();
        let activity = self
            .activity
            .acquire(generation)
            .ok_or(DeviceError::InvalidState {
                operation: "virtio-pci queue notify",
                detail: "queue generation is resetting".into(),
            })?;

        let mut queue = {
            let mut state = self.state.lock();
            // The first snapshot was taken before activity admission. A
            // reset may have completed in that gap, so validate every
            // generation- and queue-owned admission condition again while
            // holding the transport state lock. Once this permit is held, a
            // reset cannot invalidate these conditions underneath processing.
            let stale_admission = state.queue_generation != generation.value()
                || !queue_processing_enabled(state.status)
                || !dma_enabled;
            let queue = &mut state.queues[selected];
            if stale_admission || !queue.enabled || queue.processing {
                drop(state);
                drop(activity);
                return Ok(self.idle_notification());
            }
            queue.processing = true;
            queue.queue.set_ready(true);
            let queue_size = queue.queue.size;
            core::mem::replace(
                &mut queue.queue,
                VirtioQueue::new(selected as u16, queue_size, Arc::new(NoGuestMemoryAccessor)),
            )
        };

        // Queue layout validation may inspect descriptor and ring memory.
        // The transport state lock must not be held across that guest-memory
        // callback; the queue is temporarily owned by this operation instead.
        if let Err(error) = queue
            .validate_layout_with_memory(memory)
            .map_err(map_pci_error)
        {
            // Publish the fatal state before releasing the queue lease, so a
            // competing notify cannot start another operation in the gap.
            let fault = self.queue_fault(error, activity);
            self.restore_queue(selected, queue);
            return Ok(fault);
        }

        let result = self.core.notify_queue(&mut queue, memory);
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => {
                let fault = self.queue_fault(error, activity);
                self.restore_queue(selected, queue);
                return Ok(fault);
            }
        };
        if matches!(outcome, QueueNotifyOutcome::Deferred { .. }) {
            let fault = self.queue_fault(
                DeviceError::Unsupported {
                    operation: "virtio-pci queue notify",
                    detail: "asynchronous queue processing is not supported by this transport"
                        .into(),
                },
                activity,
            );
            self.restore_queue(selected, queue);
            return Ok(fault);
        }
        self.restore_queue(selected, queue);
        let kind = if matches!(outcome, QueueNotifyOutcome::Completed { notify: true }) {
            Some(InterruptPublicationKind::Queue)
        } else {
            None
        };
        Ok(VirtioPciWriteOutcome::QueueNotified(QueueNotification {
            outcome,
            publication: InterruptPublicationRequest::new(
                Arc::clone(&self.interrupts),
                kind,
                Some(activity),
            ),
        }))
    }

    fn idle_notification(&self) -> VirtioPciWriteOutcome {
        VirtioPciWriteOutcome::QueueNotified(QueueNotification {
            outcome: QueueNotifyOutcome::Idle,
            publication: InterruptPublicationRequest::new(Arc::clone(&self.interrupts), None, None),
        })
    }

    fn queue_fault(&self, error: DeviceError, activity: ActivityPermit) -> VirtioPciWriteOutcome {
        let report_interrupt = {
            let mut state = self.state.lock();
            state.device_needs_reset = true;
            state.status |= VIRTIO_STATUS_DEVICE_NEEDS_RESET as u8;
            if state.fault_reported {
                false
            } else {
                state.fault_reported = true;
                true
            }
        };
        let kind = if report_interrupt {
            Some(InterruptPublicationKind::Configuration)
        } else {
            None
        };
        VirtioPciWriteOutcome::Fault {
            error,
            publication: InterruptPublicationRequest::new(
                Arc::clone(&self.interrupts),
                kind,
                Some(activity),
            ),
        }
    }

    fn restore_queue(&self, selected: usize, queue: VirtioQueue<NoGuestMemoryAccessor>) {
        let mut state = self.state.lock();
        let queue_state = &mut state.queues[selected];
        debug_assert!(queue_state.processing);
        queue_state.queue = queue;
        queue_state.processing = false;
    }
}

fn queue_processing_enabled(status: u8) -> bool {
    let required = (VIRTIO_STATUS_FEATURES_OK | VIRTIO_STATUS_DRIVER_OK) as u8;
    let stopped = (VIRTIO_STATUS_DEVICE_NEEDS_RESET | VIRTIO_STATUS_FAILED) as u8;
    status & required == required && status & stopped == 0
}
