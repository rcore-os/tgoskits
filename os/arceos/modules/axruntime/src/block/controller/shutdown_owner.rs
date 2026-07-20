//! Final controller shutdown driven by the sole maintenance owner.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use rdif_block::{DmaQuiesced, InitError, InitInput, InitPoll, LifecycleEndpoint, RecoveryCause};

use super::{
    BlockController, BlockHandoffError, ControllerPhase, OwnerCommand, RuntimeQueue,
    source::RuntimeIrqSource,
};
use crate::block::hctx::{DrainingHardwareQueue, ServiceDrainedHardwareQueue};

/// Owner-local, linear shutdown state retained across bounded service passes.
pub(in crate::block) enum OwnerShutdown {
    Idle,
    Draining(Vec<DrainingHardwareQueue>),
    PollDma(Vec<ServiceDrainedHardwareQueue>),
}

impl OwnerShutdown {
    pub(in crate::block) const fn new() -> Self {
        Self::Idle
    }
}

pub(in crate::block) enum OwnerShutdownProgress {
    Pending { run_again: bool },
    Complete,
}

impl BlockController {
    pub(in crate::block) fn owner_shutdown_is_offline(&self) -> bool {
        self.phase() == ControllerPhase::Offline
    }

    /// Advances final shutdown without ever parking behind work owned by this
    /// same thread.
    pub(in crate::block) fn service_owner_shutdown(
        &self,
        sources: &[RuntimeIrqSource],
        shutdown: &mut OwnerShutdown,
    ) -> Result<OwnerShutdownProgress, BlockHandoffError> {
        if !matches!(shutdown, OwnerShutdown::Idle) && self.phase() == ControllerPhase::Running {
            // A fault may replace Quiescing with Recovering. Once that
            // recovery republishes Running, discard stale transition permits
            // and begin a fresh shutdown epoch.
            *shutdown = OwnerShutdown::Idle;
        }
        match shutdown {
            OwnerShutdown::Idle => self.begin_owner_shutdown(shutdown),
            OwnerShutdown::Draining(queues) => {
                if self.phase() != ControllerPhase::Quiescing {
                    return Ok(OwnerShutdownProgress::Pending { run_again: false });
                }
                if self.active_operations.load(Ordering::Acquire) != 0
                    || queues.iter().any(|queue| !queue.is_drained())
                {
                    return Ok(OwnerShutdownProgress::Pending { run_again: false });
                }
                self.begin_shutdown_dma_quiesce(sources, shutdown)
            }
            OwnerShutdown::PollDma(_) => self.poll_shutdown_dma_quiesce(shutdown),
        }
    }

    fn begin_owner_shutdown(
        &self,
        shutdown: &mut OwnerShutdown,
    ) -> Result<OwnerShutdownProgress, BlockHandoffError> {
        if self.phase() != ControllerPhase::Running
            || self.current_owner_command() != OwnerCommand::None
        {
            return Ok(OwnerShutdownProgress::Pending { run_again: false });
        }
        self.phase
            .compare_exchange(
                ControllerPhase::Running as u8,
                ControllerPhase::Quiescing as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| BlockHandoffError::InvalidState(self.name.clone()))?;

        let mut queues = Vec::new();
        for queue in self.runtime_queues() {
            match queue {
                RuntimeQueue::Inline(queue) => {
                    queue.available.store(false, Ordering::Release);
                }
                RuntimeQueue::Interrupt(queue) => queues.push(queue.begin_owner_quiesce()?),
            }
        }
        *shutdown = OwnerShutdown::Draining(queues);
        Ok(OwnerShutdownProgress::Pending { run_again: true })
    }

    fn begin_shutdown_dma_quiesce(
        &self,
        sources: &[RuntimeIrqSource],
        shutdown: &mut OwnerShutdown,
    ) -> Result<OwnerShutdownProgress, BlockHandoffError> {
        let OwnerShutdown::Draining(draining) = core::mem::replace(shutdown, OwnerShutdown::Idle)
        else {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        };
        let mut service_drained = Vec::with_capacity(draining.len());
        for queue in draining {
            service_drained.push(queue.finish()?.drain_service_work()?);
        }

        self.with_driver_endpoint_on_owner(|device| device.disable_irq())?;
        super::source::quiesce_after_device_masked(sources)?;

        if service_drained.is_empty() {
            self.close_inline_queues()?;
            self.finish_owner_shutdown()?;
            return Ok(OwnerShutdownProgress::Complete);
        }

        let epoch = self.advance_recovery_epoch()?;
        self.with_driver_endpoint_on_owner(|device| match device.bundle_mut().lifecycle() {
            LifecycleEndpoint::Inline => Err(InitError::InvalidState),
            LifecycleEndpoint::Interrupt(lifecycle) => {
                if lifecycle.controller_cookie() != self.lifecycle_cookie {
                    return Err(InitError::InvalidState);
                }
                lifecycle.begin_dma_quiesce(epoch, RecoveryCause::Shutdown)
            }
        })?;
        self.recovery_deadline_ns.store(0, Ordering::Release);
        *shutdown = OwnerShutdown::PollDma(service_drained);
        Ok(OwnerShutdownProgress::Pending { run_again: true })
    }

    fn poll_shutdown_dma_quiesce(
        &self,
        shutdown: &mut OwnerShutdown,
    ) -> Result<OwnerShutdownProgress, BlockHandoffError> {
        if self.phase() != ControllerPhase::Quiescing {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        }
        let progress =
            self.with_driver_endpoint_on_owner(|device| match device.bundle_mut().lifecycle() {
                LifecycleEndpoint::Inline => InitPoll::Failed(InitError::InvalidState),
                LifecycleEndpoint::Interrupt(lifecycle) => {
                    lifecycle.poll_dma_quiesce(InitInput::at(ax_hal::time::monotonic_time_nanos()))
                }
            });
        match progress {
            InitPoll::Ready(proof) => self.finish_shutdown_dma_quiesce(proof, shutdown),
            InitPoll::Failed(error) => Err(error.into()),
            InitPoll::Pending(schedule) => {
                let schedule = schedule.validate()?;
                if !schedule.irq_sources().is_empty() {
                    return Err(InitError::MissingInterrupt.into());
                }
                self.recovery_deadline_ns
                    .store(schedule.wake_at_ns().unwrap_or(0), Ordering::Release);
                Ok(OwnerShutdownProgress::Pending {
                    run_again: schedule.run_again(),
                })
            }
        }
    }

    fn finish_shutdown_dma_quiesce(
        &self,
        proof: DmaQuiesced,
        shutdown: &mut OwnerShutdown,
    ) -> Result<OwnerShutdownProgress, BlockHandoffError> {
        self.validate_dma_proof(&proof)?;
        let OwnerShutdown::PollDma(queues) = core::mem::replace(shutdown, OwnerShutdown::Idle)
        else {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        };
        for queue in queues {
            queue.close_after_dma_quiesce(&proof)?;
        }
        self.close_inline_queues()?;
        self.finish_owner_shutdown()?;
        Ok(OwnerShutdownProgress::Complete)
    }

    fn close_inline_queues(&self) -> Result<(), BlockHandoffError> {
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Inline(queue) = queue {
                queue.close_on_owner()?;
            }
        }
        Ok(())
    }

    fn finish_owner_shutdown(&self) -> Result<(), BlockHandoffError> {
        self.recovery_deadline_ns.store(0, Ordering::Release);
        self.phase
            .compare_exchange(
                ControllerPhase::Quiescing as u8,
                ControllerPhase::Offline as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| BlockHandoffError::InvalidState(self.name.clone()))?;
        self.operation_wait.notify_all();
        Ok(())
    }

    pub(in crate::block) fn quarantine_queue_endpoints(&self, reason: rdif_block::BlkError) {
        for queue in self.runtime_queues() {
            match queue {
                RuntimeQueue::Inline(queue) => queue.quarantine_on_owner(reason),
                RuntimeQueue::Interrupt(queue) => queue.quarantine_endpoint(reason),
            }
        }
    }
}
