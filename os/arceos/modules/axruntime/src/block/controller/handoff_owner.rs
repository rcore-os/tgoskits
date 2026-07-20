//! Bounded host-to-guest transition driven only by the maintenance owner.

use alloc::vec::Vec;
use core::sync::atomic::Ordering;

use rdif_block::{DmaQuiesced, InitError, InitInput, InitPoll, LifecycleEndpoint, RecoveryCause};

use super::{
    BlockController, BlockHandoffError, ControllerPhase, OwnerCommand, RuntimeQueue,
    irq_routes::detach_host_actions, source::RuntimeIrqSource,
};
use crate::block::hctx::{DrainingHardwareQueue, ServiceDrainedHardwareQueue};

/// Owner-local linear state that cannot be observed or advanced by producers.
pub(in crate::block) enum OwnerHandoff {
    Idle,
    Draining(Vec<DrainingHardwareQueue>),
    PollDma(Vec<ServiceDrainedHardwareQueue>),
}

impl OwnerHandoff {
    pub(in crate::block) const fn new() -> Self {
        Self::Idle
    }
}

impl BlockController {
    /// Advances at most one bounded handoff phase without parking the sole owner.
    pub(in crate::block) fn service_owner_handoff(
        &self,
        sources: &mut [RuntimeIrqSource],
        handoff: &mut OwnerHandoff,
    ) -> bool {
        if self.current_owner_command() != OwnerCommand::Handoff {
            return false;
        }

        let progress = match handoff {
            OwnerHandoff::Idle => self.begin_owner_handoff(handoff),
            OwnerHandoff::Draining(queues) => {
                if self.phase() != ControllerPhase::Quiescing {
                    Err(BlockHandoffError::InvalidState(self.name.clone()))
                } else if self.active_operations.load(Ordering::Acquire) != 0
                    || queues.iter().any(|queue| !queue.is_drained())
                {
                    return false;
                } else {
                    self.begin_owner_dma_quiesce(sources, handoff)
                }
            }
            OwnerHandoff::PollDma(_) => self.poll_owner_dma_quiesce(handoff),
        };

        match progress {
            Ok(OwnerHandoffProgress::Pending { run_again }) => run_again,
            Ok(OwnerHandoffProgress::Complete) => {
                self.finish_owner_command(Ok(()));
                false
            }
            Err(error) => {
                *handoff = OwnerHandoff::Idle;
                self.mark_offline();
                self.finish_owner_command(Err(error));
                false
            }
        }
    }

    fn begin_owner_handoff(
        &self,
        handoff: &mut OwnerHandoff,
    ) -> Result<OwnerHandoffProgress, BlockHandoffError> {
        if !self.handoff_reserved.load(Ordering::Acquire) {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
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
            if let Some(queue) = queue.interrupt_queue() {
                queues.push(queue.begin_owner_quiesce()?);
            }
        }
        *handoff = OwnerHandoff::Draining(queues);
        Ok(OwnerHandoffProgress::Pending { run_again: true })
    }

    fn begin_owner_dma_quiesce(
        &self,
        sources: &mut [RuntimeIrqSource],
        handoff: &mut OwnerHandoff,
    ) -> Result<OwnerHandoffProgress, BlockHandoffError> {
        let OwnerHandoff::Draining(draining) = core::mem::replace(handoff, OwnerHandoff::Idle)
        else {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        };
        let mut service_drained = Vec::with_capacity(draining.len());
        for queue in draining {
            service_drained.push(queue.finish()?.drain_service_work()?);
        }

        detach_host_actions(self, sources)?;
        self.recovery_irqs_enabled.store(false, Ordering::Release);
        let epoch = self.advance_recovery_epoch()?;
        self.with_driver_endpoint_on_owner(|device| match device.bundle_mut().lifecycle() {
            LifecycleEndpoint::Inline => Err(InitError::InvalidState),
            LifecycleEndpoint::Interrupt(lifecycle) => {
                if lifecycle.controller_cookie() != self.lifecycle_cookie {
                    return Err(InitError::InvalidState);
                }
                lifecycle.begin_dma_quiesce(epoch, RecoveryCause::Handoff)
            }
        })?;
        self.recovery_deadline_ns.store(0, Ordering::Release);
        *handoff = OwnerHandoff::PollDma(service_drained);
        Ok(OwnerHandoffProgress::Pending { run_again: true })
    }

    fn poll_owner_dma_quiesce(
        &self,
        handoff: &mut OwnerHandoff,
    ) -> Result<OwnerHandoffProgress, BlockHandoffError> {
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
            InitPoll::Ready(proof) => self.finish_owner_handoff(proof, handoff),
            InitPoll::Failed(error) => Err(error.into()),
            InitPoll::Pending(schedule) => {
                let schedule = schedule.validate()?;
                if !schedule.irq_sources().is_empty() {
                    return Err(InitError::MissingInterrupt.into());
                }
                self.recovery_deadline_ns
                    .store(schedule.wake_at_ns().unwrap_or(0), Ordering::Release);
                Ok(OwnerHandoffProgress::Pending {
                    run_again: schedule.run_again(),
                })
            }
        }
    }

    fn finish_owner_handoff(
        &self,
        proof: DmaQuiesced,
        handoff: &mut OwnerHandoff,
    ) -> Result<OwnerHandoffProgress, BlockHandoffError> {
        self.validate_dma_proof(&proof)?;
        let OwnerHandoff::PollDma(queues) = core::mem::replace(handoff, OwnerHandoff::Idle) else {
            return Err(BlockHandoffError::InvalidState(self.name.clone()));
        };
        for queue in queues {
            queue.detach_after_dma_quiesce(&proof)?;
        }
        self.with_driver_endpoint_on_owner(|device| match device.bundle_mut().lifecycle() {
            LifecycleEndpoint::Inline => Err(InitError::InvalidState),
            LifecycleEndpoint::Interrupt(lifecycle) => lifecycle.enter_guest_owned(proof),
        })?;
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue {
                queue.enter_guest_owned()?;
            }
        }
        self.recovery_deadline_ns.store(0, Ordering::Release);
        self.phase
            .compare_exchange(
                ControllerPhase::Quiescing as u8,
                ControllerPhase::GuestOwned as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| BlockHandoffError::InvalidState(self.name.clone()))?;
        Ok(OwnerHandoffProgress::Complete)
    }
}

enum OwnerHandoffProgress {
    Pending { run_again: bool },
    Complete,
}
