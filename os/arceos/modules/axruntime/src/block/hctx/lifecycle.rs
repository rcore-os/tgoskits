//! Queue lifecycle permits from host quiesce through recovery or guest return.

use alloc::sync::Arc;

use rdif_block::{DmaQuiesced, QueueHandle};

use super::{
    DrainingHardwareQueue, HardwareQueue, HardwareQueueError, QuarantineCompletionSink,
    QuiescedHardwareQueue, ServiceDrainedHardwareQueue,
};
use crate::block::{
    HctxCause, HctxPhase, HctxTransitionError,
    quarantine::{QueueQuarantineReservation, close_or_quarantine, quarantine_live_queue},
};

impl QuiescedHardwareQueue {
    /// Returns the queue generation held by this quiesce permit.
    pub const fn transition(&self) -> crate::block::HctxTransition {
        self.transition
    }

    /// Returns the driver queue ID owned by this permit.
    pub fn queue_id(&self) -> usize {
        self.queue.info.id
    }

    /// Converts the quiesced queue into an owner-side DMA reclaim permit.
    ///
    /// Controller handoff calls this only after device IRQ masking and OS IRQ
    /// action synchronization/detachment. The returned type is the only queue
    /// permit that can consume a controller-wide DMA-quiescence proof.
    pub(in crate::block) fn drain_service_work(
        self,
    ) -> Result<ServiceDrainedHardwareQueue, HardwareQueueError> {
        let queue = self.queue;
        Ok(ServiceDrainedHardwareQueue {
            queue,
            transition: self.transition,
        })
    }
}

impl DrainingHardwareQueue {
    /// Reports whether all accepted ownership has reached a terminal state.
    pub(in crate::block) fn is_drained(&self) -> bool {
        self.queue.is_drained()
    }

    /// Converts a completed owner-side drain into the typed quiesce proof.
    pub(in crate::block) fn finish(self) -> Result<QuiescedHardwareQueue, HardwareQueueError> {
        if !self.queue.is_drained() || self.queue.control.phase() != HctxPhase::Quiescing {
            return Err(HardwareQueueError::Offline);
        }
        Ok(QuiescedHardwareQueue {
            queue: self.queue,
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
        self.reclaim_after_dma_quiesce(proof, false)
    }

    /// Reclaims all request ownership and closes the driver endpoint after a
    /// controller-wide DMA proof during final owner shutdown.
    pub(in crate::block) fn close_after_dma_quiesce(
        self,
        proof: &DmaQuiesced,
    ) -> Result<(), HardwareQueueError> {
        self.reclaim_after_dma_quiesce(proof, true)
    }

    fn reclaim_after_dma_quiesce(
        self,
        proof: &DmaQuiesced,
        close: bool,
    ) -> Result<(), HardwareQueueError> {
        let queue = self.queue;

        // Construct the sink first so unwinding restores the driver endpoint
        // before the sink can finish any ownership accounting.
        let mut completions = QuarantineCompletionSink::new(&queue);
        let mut driver = queue.take_driver_on_owner()?;
        driver.reclaim_after_quiesce(proof, &mut completions)?;
        drop(driver);
        completions.finish()?;
        if let Err(error) = queue.control.finish_detach(self.transition) {
            let _ = queue.control.mark_offline();
            return Err(error.into());
        }
        if close {
            let driver = queue.take_driver_on_owner()?.into_inner();
            let reservation = queue.take_quarantine_reservation()?;
            let close_result = close_or_quarantine(driver, reservation);
            close_result?;
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

    pub(in crate::block) fn quarantine_endpoint(&self, reason: rdif_block::BlkError) {
        let queue = self.queue.lock().take();
        if let Some(queue) = queue {
            let reservation = self
                .take_quarantine_reservation()
                .expect("live block hctx must retain its quarantine reservation");
            quarantine_live_queue(queue, reason, reservation);
        }
    }

    pub(in crate::block) fn abort_unpublished_after_irq_quiesce(&self) {
        let queue = self;
        queue.mark_offline();
        let driver = queue.queue.lock().take();
        let shutdown_failed = driver.is_some_and(|driver| {
            let reservation = queue
                .take_quarantine_reservation()
                .expect("unpublished hctx must retain its quarantine reservation");
            close_or_quarantine(driver, reservation).is_err()
        });
        if shutdown_failed {
            error!(
                "unpublished block hctx {} could not return driver ownership",
                queue.info.id
            );
        }
    }

    /// Closes admission without waiting for the owner thread's own service.
    pub(in crate::block) fn begin_owner_quiesce(
        self: &Arc<Self>,
    ) -> Result<DrainingHardwareQueue, HardwareQueueError> {
        let queue = self.as_ref();
        let transition = queue.control.begin_quiesce()?;
        if queue.is_drained() {
            queue.drain_wait.notify_all();
        } else if let Err(error) = queue.queue_service(HctxCause::Shutdown) {
            let _ = queue.control.mark_offline();
            queue.drain_wait.notify_all();
            return Err(error);
        }
        Ok(DrainingHardwareQueue {
            queue: Arc::clone(self),
            transition,
        })
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

    fn take_quarantine_reservation(
        &self,
    ) -> Result<QueueQuarantineReservation, HardwareQueueError> {
        self.quarantine_reservation
            .lock()
            .take()
            .ok_or(HardwareQueueError::Offline)
    }
}

impl Drop for HardwareQueue {
    fn drop(&mut self) {
        if let Some(queue) = self.queue.get_mut().take() {
            error!(
                "block hctx {} dropped before owner teardown; retaining endpoint fail-closed",
                self.info.id
            );
            let reservation = self
                .quarantine_reservation
                .get_mut()
                .take()
                .expect("a live block hctx must retain its quarantine reservation");
            quarantine_live_queue(queue, rdif_block::BlkError::Quarantined, reservation);
        }

        let reservation = self
            .completion_quarantine_reservation
            .get_mut()
            .take()
            .expect("a live block hctx must retain completion quarantine capacity");
        let quarantine = self
            .rejected_completions
            .get_mut()
            .take()
            .expect("a live block hctx must own rejected completion storage");
        if quarantine.has_retained() {
            reservation.retain(quarantine);
        } else {
            reservation.release();
        }
    }
}

pub(super) fn shutdown_unpublished_queue(
    queue: QueueHandle,
    reservation: QueueQuarantineReservation,
) {
    let shutdown_failed = close_or_quarantine(queue, reservation).is_err();
    if shutdown_failed {
        // QueueHandle owns the one-shot shutdown transaction. Its Drop keeps
        // an endpoint whose shutdown attempt failed alive for shutdown
        // lifetime, while this helper retains only the unrepresentable extra
        // completion owner.
        error!("unpublished block queue could not complete its shutdown transaction");
    }
}
