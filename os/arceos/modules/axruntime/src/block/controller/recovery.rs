//! Recovery and guest-return transitions executed only by the maintenance owner.

use alloc::sync::Arc;
use core::sync::atomic::Ordering;

use rdif_block::{
    ControllerEpoch, ControllerReady, DmaQuiesced, IdList, InitError, InitInput, InitPoll,
    LifecycleEndpoint, RecoveryCause,
};

use super::{
    BlockController, BlockHandoffError, ControllerPhase, OwnerCommand, RuntimeQueue,
    irq_routes::reattach_host_actions,
    source::{RuntimeIrqSource, RuntimeIrqSourceError, runtime_irq_source_mut},
};

const RECOVERY_TRANSITION_BUDGET: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum RecoveryStep {
    Idle                = 0,
    DisableActions      = 1,
    BeginQuiesce        = 2,
    PollQuiesce         = 3,
    EnableReinitActions = 4,
    PollReinitialize    = 5,
    PublishRunning      = 6,
    Finished            = 7,
}

impl RecoveryStep {
    fn decode(value: u8) -> Self {
        match value {
            1 => Self::DisableActions,
            2 => Self::BeginQuiesce,
            3 => Self::PollQuiesce,
            4 => Self::EnableReinitActions,
            5 => Self::PollReinitialize,
            6 => Self::PublishRunning,
            7 => Self::Finished,
            _ => Self::Idle,
        }
    }
}

impl BlockController {
    /// Publishes a recovery request; the fixed owner performs every device step.
    pub(super) fn schedule_recovery(&self, cause: RecoveryCause) {
        let transitioned = loop {
            let observed = self.phase();
            if observed == ControllerPhase::Recovering {
                return;
            }
            if !matches!(
                observed,
                ControllerPhase::Running | ControllerPhase::Quiescing
            ) {
                return;
            }
            if self
                .phase
                .compare_exchange_weak(
                    observed as u8,
                    ControllerPhase::Recovering as u8,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                break true;
            }
        };
        if !transitioned {
            return;
        }
        if self.advance_recovery_epoch().is_err() {
            self.mark_offline();
            return;
        }
        *self.recovery_cause.lock() = Some(cause);
        self.reset_recovery_inputs();
        self.recovery_step
            .store(RecoveryStep::DisableActions as u8, Ordering::Release);
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue {
                queue.close_access_for_recovery();
            }
        }
    }

    pub(super) fn publish_irq_recovery(&self, queue_id: usize) -> bool {
        if queue_id >= u64::BITS as usize {
            return false;
        }
        self.irq_recovery_queues
            .fetch_or(1_u64 << queue_id, Ordering::Release);
        true
    }

    pub(super) fn record_recovery_irq(&self, source_id: usize) -> bool {
        if source_id >= u64::BITS as usize || self.phase() != ControllerPhase::Recovering {
            return false;
        }
        self.recovery_pending_sources
            .fetch_or(1_u64 << source_id, Ordering::Release);
        true
    }

    /// Starts or completes the asynchronous guest-return command.
    pub(in crate::block) fn service_owner_return(&self, sources: &mut [RuntimeIrqSource]) -> bool {
        match self.current_owner_command() {
            OwnerCommand::ReturnHost => match self.begin_return_from_guest(sources) {
                Ok(()) => {
                    self.mark_owner_return_waiting();
                    true
                }
                Err(error) => {
                    self.mark_offline();
                    self.finish_owner_command(Err(error));
                    false
                }
            },
            OwnerCommand::ReturnWaiting => match self.phase() {
                ControllerPhase::Running => {
                    self.finish_owner_command(Ok(()));
                    false
                }
                ControllerPhase::Offline => {
                    self.finish_owner_command(Err(BlockHandoffError::GuestReturn(
                        self.name.clone(),
                    )));
                    false
                }
                _ => false,
            },
            OwnerCommand::None | OwnerCommand::Handoff | OwnerCommand::Preparing => false,
        }
    }

    /// Performs a bounded recovery pass and reports immediately runnable work.
    pub(in crate::block) fn service_owner_recovery(
        &self,
        sources: &mut [RuntimeIrqSource],
    ) -> Result<bool, InitError> {
        let pending_irq_queues = self.irq_recovery_queues.swap(0, Ordering::AcqRel);
        if pending_irq_queues != 0 && self.phase() == ControllerPhase::Running {
            self.schedule_recovery(RecoveryCause::QueueFault {
                queue_id: pending_irq_queues.trailing_zeros() as usize,
            });
        }
        if self.phase() != ControllerPhase::Recovering {
            return Ok(false);
        }

        for _ in 0..RECOVERY_TRANSITION_BUDGET {
            match RecoveryStep::decode(self.recovery_step.load(Ordering::Acquire)) {
                RecoveryStep::Idle | RecoveryStep::Finished => return Ok(false),
                RecoveryStep::DisableActions => {
                    self.with_driver_endpoint_on_owner(|device| device.disable_irq())
                        .map_err(|_| {
                            InitError::Hardware("recovery could not mask device IRQ sources")
                        })?;
                    super::source::quiesce_after_device_masked(sources).map_err(|_| {
                        InitError::Hardware(
                            "recovery could not quiesce IRQ actions after masking the device",
                        )
                    })?;
                    // The fully masked and synchronized device establishes the
                    // retirement boundary for generation-bearing masks. The
                    // following reset/reinitialization creates a new epoch.
                    for source in sources.iter_mut() {
                        source.discard_ledger_after_device_quiesce();
                    }
                    self.recovery_irqs_enabled.store(false, Ordering::Release);
                    self.recovery_step
                        .store(RecoveryStep::BeginQuiesce as u8, Ordering::Release);
                }
                RecoveryStep::BeginQuiesce => {
                    let epoch = self.expected_recovery_epoch();
                    let cause = (*self.recovery_cause.lock()).ok_or(InitError::InvalidState)?;
                    self.with_driver_endpoint_on_owner(|device| {
                        match device.bundle_mut().lifecycle() {
                            LifecycleEndpoint::Inline => Err(InitError::InvalidState),
                            LifecycleEndpoint::Interrupt(lifecycle) => {
                                lifecycle.begin_dma_quiesce(epoch, cause)
                            }
                        }
                    })?;
                    self.recovery_step
                        .store(RecoveryStep::PollQuiesce as u8, Ordering::Release);
                }
                RecoveryStep::PollQuiesce => {
                    let (input, _) = self.take_recovery_input();
                    let progress = self.with_driver_endpoint_on_owner(|device| {
                        match device.bundle_mut().lifecycle() {
                            LifecycleEndpoint::Inline => InitPoll::Failed(InitError::InvalidState),
                            LifecycleEndpoint::Interrupt(lifecycle) => {
                                lifecycle.poll_dma_quiesce(input)
                            }
                        }
                    });
                    match progress {
                        InitPoll::Ready(proof) => self.begin_owner_reinitialize(proof)?,
                        InitPoll::Failed(error) => return Err(error),
                        InitPoll::Pending(schedule) => {
                            let schedule = schedule.validate()?;
                            if !schedule.irq_sources().is_empty() {
                                return Err(InitError::MissingInterrupt);
                            }
                            return self.arm_recovery_schedule(schedule);
                        }
                    }
                }
                RecoveryStep::EnableReinitActions => {
                    for source in sources.iter() {
                        source.enable().map_err(|_| {
                            InitError::Hardware("recovery could not enable an IRQ action")
                        })?;
                    }
                    self.with_driver_endpoint_on_owner(|device| device.enable_irq())
                        .map_err(|_| {
                            InitError::Hardware("recovery could not unmask reinitialization IRQs")
                        })?;
                    self.recovery_irqs_enabled.store(true, Ordering::Release);
                    self.recovery_step
                        .store(RecoveryStep::PollReinitialize as u8, Ordering::Release);
                }
                RecoveryStep::PollReinitialize => {
                    let (input, consumed) = self.take_recovery_input();
                    let progress = self.with_driver_endpoint_on_owner(|device| {
                        match device.bundle_mut().lifecycle() {
                            LifecycleEndpoint::Inline => InitPoll::Failed(InitError::InvalidState),
                            LifecycleEndpoint::Interrupt(lifecycle) => {
                                lifecycle.poll_reinitialize(input)
                            }
                        }
                    });
                    match progress {
                        InitPoll::Ready(proof) => {
                            self.validate_ready_proof(&proof)?;
                            self.rearm_consumed_sources(sources, consumed)?;
                            for queue in self.runtime_queues() {
                                if let RuntimeQueue::Interrupt(queue) = queue {
                                    queue
                                        .finish_reinitialization()
                                        .map_err(|_| InitError::InvalidState)?;
                                }
                            }
                            self.recovery_step
                                .store(RecoveryStep::PublishRunning as u8, Ordering::Release);
                        }
                        InitPoll::Failed(error) => return Err(error),
                        InitPoll::Pending(schedule) => {
                            let schedule = schedule.validate()?;
                            self.rearm_consumed_sources(sources, consumed)?;
                            return self.arm_recovery_schedule(schedule);
                        }
                    }
                }
                RecoveryStep::PublishRunning => {
                    self.phase
                        .compare_exchange(
                            ControllerPhase::Recovering as u8,
                            ControllerPhase::Running as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .map_err(|_| InitError::InvalidState)?;
                    self.reset_recovery_inputs();
                    *self.recovery_cause.lock() = None;
                    self.recovery_step
                        .store(RecoveryStep::Finished as u8, Ordering::Release);
                    self.operation_wait.notify_all();
                    return Ok(false);
                }
            }
        }
        Ok(true)
    }

    fn begin_owner_reinitialize(&self, proof: DmaQuiesced) -> Result<(), InitError> {
        self.validate_dma_proof(&proof)?;
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue {
                queue
                    .reclaim_after_quiesce(&proof)
                    .map_err(|_| InitError::InvalidState)?;
                queue
                    .begin_reinitialization()
                    .map_err(|_| InitError::InvalidState)?;
            }
        }
        self.with_driver_endpoint_on_owner(|device| match device.bundle_mut().lifecycle() {
            LifecycleEndpoint::Inline => Err(InitError::InvalidState),
            LifecycleEndpoint::Interrupt(lifecycle) => lifecycle.begin_reinitialize(proof),
        })?;
        self.recovery_step
            .store(RecoveryStep::EnableReinitActions as u8, Ordering::Release);
        Ok(())
    }

    fn take_recovery_input(&self) -> (InitInput, IdList) {
        self.recovery_wait_sources.store(0, Ordering::Release);
        self.recovery_deadline_ns.store(0, Ordering::Release);
        let consumed = IdList::from_bits(self.recovery_pending_sources.swap(0, Ordering::AcqRel));
        (
            InitInput::new(ax_hal::time::monotonic_time_nanos(), consumed),
            consumed,
        )
    }

    fn arm_recovery_schedule(&self, schedule: rdif_block::InitSchedule) -> Result<bool, InitError> {
        let schedule = schedule.validate()?;
        self.recovery_wait_sources
            .store(schedule.irq_sources().bits(), Ordering::Release);
        self.recovery_deadline_ns
            .store(schedule.wake_at_ns().unwrap_or(0), Ordering::Release);
        let pending = self.recovery_pending_sources.load(Ordering::Acquire)
            & schedule.irq_sources().bits()
            != 0;
        Ok(schedule.run_again() || pending)
    }

    fn rearm_consumed_sources(
        &self,
        sources: &mut [RuntimeIrqSource],
        consumed: IdList,
    ) -> Result<(), InitError> {
        for source_id in consumed.iter() {
            let source = runtime_irq_source_mut(sources, source_id).map_err(|error| {
                recovery_source_error("recovery IRQ source lookup failed", error)
            })?;
            source.finish_service();
            source.rearm_retained().map_err(|error| {
                recovery_source_error("recovery IRQ source rearm failed", error)
            })?;
        }
        Ok(())
    }

    fn begin_return_from_guest(
        &self,
        sources: &mut [RuntimeIrqSource],
    ) -> Result<(), BlockHandoffError> {
        self.phase
            .compare_exchange(
                ControllerPhase::GuestOwned as u8,
                ControllerPhase::Recovering as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| BlockHandoffError::InvalidState(self.name.clone()))?;
        reattach_host_actions(sources)?;
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue {
                queue.begin_guest_return_recovery()?;
            }
        }
        self.advance_recovery_epoch()?;
        *self.recovery_cause.lock() = Some(RecoveryCause::Handoff);
        self.reset_recovery_inputs();
        self.recovery_step
            .store(RecoveryStep::DisableActions as u8, Ordering::Release);
        Ok(())
    }

    pub(super) fn return_from_guest(self: &Arc<Self>) -> Result<(), BlockHandoffError> {
        self.request_owner_command(OwnerCommand::ReturnHost)
    }

    pub(in crate::block) fn mark_offline(&self) {
        let previous = ControllerPhase::decode(
            self.phase
                .swap(ControllerPhase::Offline as u8, Ordering::AcqRel),
        );
        if previous == ControllerPhase::Offline {
            return;
        }
        self.reset_recovery_inputs();
        self.recovery_step
            .store(RecoveryStep::Finished as u8, Ordering::Release);
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue {
                queue.mark_offline();
            }
        }
        self.operation_wait.notify_all();
    }

    fn reset_recovery_inputs(&self) {
        self.recovery_pending_sources.store(0, Ordering::Release);
        self.recovery_wait_sources.store(0, Ordering::Release);
        self.recovery_deadline_ns.store(0, Ordering::Release);
    }

    fn expected_recovery_epoch(&self) -> ControllerEpoch {
        ControllerEpoch::new(self.recovery_epoch.load(Ordering::Acquire))
    }

    pub(super) fn advance_recovery_epoch(&self) -> Result<ControllerEpoch, InitError> {
        self.recovery_epoch
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |epoch| {
                epoch.checked_add(1)
            })
            .map(|previous| ControllerEpoch::new(previous + 1))
            .map_err(|_| InitError::InvalidState)
    }

    pub(super) fn validate_dma_proof(&self, proof: &DmaQuiesced) -> Result<(), InitError> {
        if proof.epoch() != self.expected_recovery_epoch()
            || proof.controller_cookie() != self.lifecycle_cookie
        {
            return Err(InitError::InvalidState);
        }
        Ok(())
    }

    fn validate_ready_proof(&self, proof: &ControllerReady) -> Result<(), InitError> {
        if proof.epoch() != self.expected_recovery_epoch()
            || proof.controller_cookie() != self.lifecycle_cookie
        {
            return Err(InitError::InvalidState);
        }
        Ok(())
    }
}

fn recovery_source_error(context: &'static str, error: RuntimeIrqSourceError) -> InitError {
    error!("{context}: {error}");
    match error {
        RuntimeIrqSourceError::UnknownSource { .. } => InitError::MissingInterrupt,
        RuntimeIrqSourceError::ConflictingGeneration { .. }
        | RuntimeIrqSourceError::ConflictingMaskEpoch { .. }
        | RuntimeIrqSourceError::ServicePending { .. }
        | RuntimeIrqSourceError::Rearm { .. } => InitError::Hardware(context),
    }
}
