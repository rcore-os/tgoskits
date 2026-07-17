//! Queue lifecycle permits from host quiesce through recovery or guest return.

use core::{mem::ManuallyDrop, pin::Pin, sync::atomic::Ordering};

use rdif_block::{DmaQuiesced, QueueHandle};

use super::{
    CompletionBatch, HardwareQueue, HardwareQueueError, QuiescedHardwareQueue,
    ServiceDrainedHardwareQueue,
};
use crate::block::{HctxCause, HctxPhase, HctxTransitionError};

impl QuiescedHardwareQueue {
    /// Returns the queue generation held by this quiesce permit.
    pub const fn transition(&self) -> crate::block::HctxTransition {
        self.transition
    }

    /// Returns the driver queue ID owned by this permit.
    pub fn queue_id(&self) -> usize {
        self.queue.info.id
    }

    /// Cancels the watchdog and waits for the fixed service callback to exit.
    ///
    /// Controller handoff calls this only after device IRQ masking and OS IRQ
    /// action synchronization/detachment. The returned type is the only queue
    /// permit that can consume a controller-wide DMA-quiescence proof.
    pub(in crate::block) fn drain_service_work(
        self,
    ) -> Result<ServiceDrainedHardwareQueue, HardwareQueueError> {
        let queue = self.queue;
        queue
            .work_domain
            .cancel_delayed_work_sync(queue.watchdog_work())?;
        queue.work_domain.flush_work(queue.service_work())?;
        Ok(ServiceDrainedHardwareQueue {
            queue,
            transition: self.transition,
        })
    }
}

impl ServiceDrainedHardwareQueue {
    /// Reclaims host request ownership after controller-wide DMA quiescence.
    pub(in crate::block) fn detach_after_dma_quiesce(
        self,
        proof: &DmaQuiesced,
    ) -> Result<(), HardwareQueueError> {
        let queue = self.queue;

        let mut completions = CompletionBatch::new();
        queue
            .queue
            .lock()
            .reclaim_after_quiesce(proof, &mut completions)?;
        let completion_result = completions.drain_with(|completion| {
            Err(queue.retain_failed_completion(HardwareQueueError::StaleCompletion, completion))
        });
        let overflow_result = completions.take_overflow().map(|completion| {
            queue.retain_failed_completion(HardwareQueueError::Capacity, completion)
        });
        completion_result?;
        if let Some(error) = overflow_result {
            return Err(error);
        }
        if let Err(error) = queue.control.finish_detach(self.transition) {
            let _ = queue.control.mark_offline();
            return Err(error.into());
        }
        Ok(())
    }
}

impl HardwareQueue {
    pub(in crate::block) fn mark_offline(&self) {
        self.access_gate.close();
        let _ = self.control.mark_offline();
        self.requests.notify_all_waiters_offline();
        self.drain_wait.notify_all();
    }

    pub(in crate::block) fn abort_unpublished_after_irq_quiesce(self: Pin<&'static Self>) {
        let queue = self.get_ref();
        queue.mark_offline();
        let _ = queue
            .work_domain
            .cancel_delayed_work_sync(queue.watchdog_work());
        let _ = queue.work_domain.cancel_work_sync(queue.service_work());
        let mut completions = CompletionBatch::new();
        let shutdown_failed = queue.queue.lock().shutdown(&mut completions).is_err();
        let overflowed = completions.overflowed();
        let _poison_owner = completions.take_overflow().map(ManuallyDrop::new);
        if shutdown_failed || overflowed {
            error!(
                "unpublished block hctx {} could not return driver ownership",
                queue.info.id
            );
        }
    }

    /// Closes admission and waits for accepted requests to finish through IRQ.
    pub fn quiesce_and_drain(
        self: Pin<&'static Self>,
    ) -> Result<QuiescedHardwareQueue, HardwareQueueError> {
        let queue = self.get_ref();
        let transition = queue.control.begin_quiesce()?;
        if queue.is_drained() {
            queue.drain_wait.notify_all();
        } else if let Err(error) = queue.queue_service(HctxCause::Shutdown) {
            let _ = queue.control.mark_offline();
            queue.drain_wait.notify_all();
            return Err(error);
        }
        queue.drain_wait.try_wait_until(|| {
            queue.is_drained() || queue.control.phase() != HctxPhase::Quiescing
        })?;
        if !queue.is_drained() || queue.control.phase() != HctxPhase::Quiescing {
            return Err(HardwareQueueError::Offline);
        }
        Ok(QuiescedHardwareQueue { queue, transition })
    }

    pub(in crate::block) fn close_access_for_recovery(&self) {
        if matches!(
            self.control.phase(),
            HctxPhase::Running | HctxPhase::Quiescing
        ) {
            let _ = self.control.begin_recovery();
        }
        self.access_gate.close();
        self.drain_wait.notify_all();
    }

    pub(in crate::block) fn access_is_drained(&self) -> bool {
        self.access_gate.is_drained()
    }

    pub(in crate::block) fn is_fatal_completion_quarantined(&self) -> bool {
        self.fatal_completion_quarantine.load(Ordering::Acquire)
    }

    pub(in crate::block) fn begin_reinitialization(&self) -> Result<(), HardwareQueueError> {
        let recovery = self.control.recovery_transition()?;
        self.control.begin_reinitialization(recovery)?;
        Ok(())
    }

    pub(in crate::block) fn finish_reinitialization(&self) -> Result<(), HardwareQueueError> {
        self.access_gate.reopen()?;
        if let Err(error) = self.control.finish_reinitialization() {
            self.access_gate.close();
            return Err(error.into());
        }
        Ok(())
    }

    pub(in crate::block) fn enter_guest_owned(&self) -> Result<(), HardwareQueueError> {
        self.control.enter_guest_owned()?;
        Ok(())
    }

    pub(in crate::block) fn begin_guest_return_recovery(&self) -> Result<(), HardwareQueueError> {
        self.access_gate.close();
        if !self.access_gate.is_drained() {
            return Err(HardwareQueueError::Lifecycle(
                HctxTransitionError::InvalidTransition,
            ));
        }
        if let Err(error) = self.control.begin_guest_return_recovery() {
            let _ = self.access_gate.reopen();
            return Err(error.into());
        }
        Ok(())
    }
}

pub(super) fn shutdown_unpublished_queue(mut queue: QueueHandle) {
    let mut completions = CompletionBatch::new();
    let shutdown_failed = queue.shutdown(&mut completions).is_err();
    let overflowed = completions.overflowed();
    let _poison_owner = completions.take_overflow().map(ManuallyDrop::new);
    if shutdown_failed || overflowed {
        // QueueHandle owns the one-shot shutdown transaction. Its Drop keeps
        // an endpoint whose shutdown attempt failed alive for shutdown
        // lifetime, while this helper retains only the unrepresentable extra
        // completion owner.
        error!("unpublished block queue could not complete its shutdown transaction");
    }
}
