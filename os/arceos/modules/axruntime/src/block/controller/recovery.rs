//! Bounded controller recovery, IRQ draining, and guest-return reinitialization.

use alloc::sync::Arc;
use core::{pin::Pin, ptr, sync::atomic::Ordering};

use rdif_block::{
    ControllerEpoch, ControllerReady, DmaQuiesced, InitError, InitPoll, LifecycleEndpoint,
    RecoveryCause,
};

use super::{
    BlockController, BlockHandoffError, ControllerOwnerLink, ControllerPhase, MAX_HARDWARE_QUEUES,
    RuntimeQueue, recovery_irq::release_registration_quenches,
    rollback_unpublished_runtime_devices,
};
use crate::{
    block::HardwareQueueError,
    workqueue::{DelayedWork, WorkItem, WorkOutcome},
};

pub(super) fn controller_recovery_work_entry(data: usize) -> WorkOutcome {
    let link = unsafe {
        // SAFETY: callback data names the shutdown-lifetime owner link that
        // embeds this work item's only controller publication slot.
        &*ptr::with_exposed_provenance::<ControllerOwnerLink>(data)
    };
    let owner = link.owner.load(Ordering::Acquire);
    if owner.is_null() {
        return WorkOutcome::Complete;
    }
    let controller = unsafe {
        // SAFETY: owner publication and clearing follow the contract described
        // by ControllerOwnerLink; callback drain precedes pointer clearing.
        &*owner
    };
    if let Some(cause) = controller.take_irq_recovery() {
        controller.schedule_recovery(cause);
    }
    controller.recover_bounded()
}

pub(super) fn controller_recovery_timer_entry(data: usize) -> WorkOutcome {
    let link = unsafe {
        // SAFETY: callback data names the shutdown-lifetime controller owner
        // link shared with the ordinary recovery work item.
        &*ptr::with_exposed_provenance::<ControllerOwnerLink>(data)
    };
    link.wake_recovery();
    WorkOutcome::Complete
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(super) enum RecoveryStep {
    Idle             = 0,
    DisableActions   = 1,
    DrainActions     = 2,
    BeginQuiesce     = 3,
    PollQuiesce      = 4,
    PollReinitialize = 5,
    EnableActions    = 6,
    Finished         = 7,
}

impl RecoveryStep {
    fn decode(value: u8) -> Self {
        match value {
            1 => Self::DisableActions,
            2 => Self::DrainActions,
            3 => Self::BeginQuiesce,
            4 => Self::PollQuiesce,
            5 => Self::PollReinitialize,
            6 => Self::EnableActions,
            7 => Self::Finished,
            _ => Self::Idle,
        }
    }
}

impl BlockController {
    /// Latches one queue fault and activates only fixed recovery work from
    /// hard IRQ. Lifecycle transitions, waiter notification, and driver calls
    /// remain in [`controller_recovery_work_entry`].
    pub(super) fn publish_irq_recovery(&'static self, queue_id: usize) -> bool {
        if queue_id >= u64::BITS as usize {
            return false;
        }
        self.irq_recovery_queues
            .fetch_or(1_u64 << queue_id, Ordering::Release);
        self.queue_recovery_work().is_ok()
    }

    fn take_irq_recovery(&self) -> Option<RecoveryCause> {
        let queues = self.irq_recovery_queues.swap(0, Ordering::AcqRel);
        (queues != 0).then(|| RecoveryCause::QueueFault {
            queue_id: queues.trailing_zeros() as usize,
        })
    }

    pub(super) fn return_from_guest(self: &Arc<Self>) -> Result<(), BlockHandoffError> {
        self.phase
            .compare_exchange(
                ControllerPhase::GuestOwned as u8,
                ControllerPhase::Recovering as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .map_err(|_| BlockHandoffError::InvalidState(self.name.clone()))?;

        if let Err(error) = self.reattach_host_actions() {
            self.mark_offline();
            return Err(error);
        }
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue
                && let Err(error) = queue.begin_guest_return_recovery()
            {
                self.mark_offline();
                return Err(error.into());
            }
        }
        if self.advance_recovery_epoch().is_err() {
            self.mark_offline();
            return Err(BlockHandoffError::GuestReturn(self.name.clone()));
        }

        *self.recovery_cause.lock() = Some(RecoveryCause::Handoff);
        self.recovery_failed.store(false, Ordering::Release);
        self.irq_recovery_queues.store(0, Ordering::Release);
        self.recovery_wait_sources.store(0, Ordering::Release);
        self.recovery_pending_sources.store(0, Ordering::Release);
        self.recovery_polling_irqs.store(false, Ordering::Release);
        self.recovery_irq_drains.lock().fill(None);
        self.recovery_step
            .store(RecoveryStep::DisableActions as u8, Ordering::Release);

        let controller: &'static Self = unsafe {
            // SAFETY: handoff tokens are created only from controllers in the
            // shutdown-lifetime runtime registry. The Arc in this token also
            // remains live until this recovery work reaches a terminal phase.
            &*Arc::as_ptr(self)
        };
        if controller.queue_recovery_work().is_err() {
            controller.mark_offline();
            return Err(BlockHandoffError::GuestReturn(controller.name.clone()));
        }
        controller.operation_wait.try_wait_until(|| {
            matches!(
                controller.phase(),
                ControllerPhase::Running | ControllerPhase::Offline
            )
        })?;
        match controller.phase() {
            ControllerPhase::Running => Ok(()),
            _ => Err(BlockHandoffError::GuestReturn(controller.name.clone())),
        }
    }

    pub(super) fn mark_offline(&self) {
        self.phase
            .swap(ControllerPhase::Offline as u8, Ordering::AcqRel);
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue {
                queue.mark_offline();
            }
        }
        self.operation_wait.notify_all();
    }

    pub(super) fn schedule_recovery(&'static self, cause: RecoveryCause) {
        let mut recovery_cause = self.recovery_cause.lock();
        let mut observed = self.phase.load(Ordering::Acquire);
        loop {
            let phase = ControllerPhase::decode(observed);
            if matches!(
                phase,
                ControllerPhase::Recovering | ControllerPhase::Offline
            ) {
                return;
            }
            if !matches!(phase, ControllerPhase::Running | ControllerPhase::Quiescing) {
                return;
            }

            // Clear evidence from the previous controller epoch while IRQ
            // callbacks still observe a non-recovery phase. Once Recovering is
            // published, every newly accepted source survives until a bounded
            // worker consumes or explicitly discards it.
            self.recovery_failed.store(false, Ordering::Release);
            self.recovery_wait_sources.store(0, Ordering::Release);
            self.recovery_pending_sources.store(0, Ordering::Release);
            self.recovery_polling_irqs.store(false, Ordering::Release);
            match self.phase.compare_exchange_weak(
                observed,
                ControllerPhase::Recovering as u8,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(actual) => observed = actual,
            }
        }

        if self.advance_recovery_epoch().is_err() {
            drop(recovery_cause);
            self.mark_offline();
            error!("block controller {} exhausted recovery epochs", self.name);
            return;
        }
        *recovery_cause = Some(cause);
        self.recovery_step
            .store(RecoveryStep::DisableActions as u8, Ordering::Release);
        drop(recovery_cause);
        for queue in self.runtime_queues() {
            if let RuntimeQueue::Interrupt(queue) = queue {
                queue.close_access_for_recovery();
            }
        }

        if let Err(error) = self.queue_recovery_work() {
            // Work admission is a runtime invariant. Mask the device and
            // retain every IRQ/DMA owner rather than leaving a live source or
            // fabricating request completion.
            if !self.mask_recovery_sources() {
                error!(
                    "block controller {} retained live IRQ actions after recovery work admission \
                     failed",
                    self.name
                );
            }
            self.mark_offline();
            error!(
                "block controller {} could not queue recovery for {cause:?}: {error}",
                self.name,
            );
        }
    }

    fn recovery_work(&'static self) -> Pin<&'static WorkItem> {
        unsafe {
            // SAFETY: successful controllers are retained by the runtime
            // registry until shutdown. Failed activation drains this work
            // before clearing the owner link and releasing its Arc.
            Pin::new_unchecked(&self.recovery_work)
        }
    }

    pub(super) fn recovery_timer(&'static self) -> Pin<&'static DelayedWork> {
        unsafe {
            // SAFETY: the timer is embedded in the same shutdown-lifetime,
            // pinned controller as the ordinary recovery work item.
            Pin::new_unchecked(&self.recovery_timer)
        }
    }

    pub(super) fn queue_recovery_work(
        &'static self,
    ) -> Result<(), crate::workqueue::WorkQueueError> {
        let _ = self.recovery_domain.queue_work_on(self.recovery_work())?;
        Ok(())
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

    fn recover_bounded(&'static self) -> WorkOutcome {
        if self.recovery_timer.take_failure().is_some() {
            self.finish_failed_recovery_init(
                "could not deliver a recovery deadline",
                InitError::TimedOut,
            );
            return WorkOutcome::Complete;
        }
        for _ in 0..64 {
            match RecoveryStep::decode(self.recovery_step.load(Ordering::Acquire)) {
                RecoveryStep::DisableActions => {
                    if self.runtime_queues().any(|queue| {
                        matches!(queue, RuntimeQueue::Interrupt(queue) if !queue.access_is_drained())
                    }) {
                        // The final hctx accessor queues the recovery work.
                        return WorkOutcome::Complete;
                    }
                    if let Err(error) = self.device.lock().disable_irq() {
                        // Actions remain live because an unmasked level source
                        // still needs an acknowledgement owner.
                        error!(
                            "block controller {} could not mask device IRQ delivery: {error}",
                            self.name
                        );
                        self.mark_offline();
                        self.recovery_step
                            .store(RecoveryStep::Finished as u8, Ordering::Release);
                        return WorkOutcome::Complete;
                    }
                    self.recovery_irqs_enabled.store(false, Ordering::Release);
                    if !self.finish_masked_source_continuations() {
                        // A source worker still owns its linear token. It wakes
                        // recovery after finishing or restoring that token.
                        return WorkOutcome::Complete;
                    }
                    let registration_guard = self.registrations.lock();
                    let Some(registrations) = registration_guard.as_ref() else {
                        self.recovery_step
                            .store(RecoveryStep::BeginQuiesce as u8, Ordering::Release);
                        continue;
                    };
                    if let Err(error) = release_registration_quenches(registrations) {
                        drop(registration_guard);
                        self.mark_offline();
                        self.recovery_step
                            .store(RecoveryStep::Finished as u8, Ordering::Release);
                        error!(
                            "block controller {} could not release its quenched IRQ line: \
                             {error:?}",
                            self.name
                        );
                        return WorkOutcome::Complete;
                    }
                    if registrations.len() > MAX_HARDWARE_QUEUES {
                        self.mark_offline();
                        self.recovery_step
                            .store(RecoveryStep::Finished as u8, Ordering::Release);
                        error!(
                            "block controller {} exposed too many IRQ actions",
                            self.name
                        );
                        return WorkOutcome::Complete;
                    }

                    let mut drains = self.recovery_irq_drains.lock();
                    for (index, registration) in registrations.iter().enumerate() {
                        if drains[index].is_none() {
                            match registration.disable_async(self.irq_drain_wake()) {
                                Ok(token) => drains[index] = Some(token),
                                Err(error) => {
                                    drop(drains);
                                    drop(registration_guard);
                                    self.mark_offline();
                                    self.recovery_step
                                        .store(RecoveryStep::Finished as u8, Ordering::Release);
                                    error!(
                                        "block controller {} could not disable IRQ action: \
                                         {error:?}",
                                        self.name
                                    );
                                    return WorkOutcome::Complete;
                                }
                            }
                        }
                    }
                    self.recovery_step
                        .store(RecoveryStep::DrainActions as u8, Ordering::Release);
                }
                RecoveryStep::DrainActions => {
                    let registration_guard = self.registrations.lock();
                    let Some(registrations) = registration_guard.as_ref() else {
                        self.recovery_step
                            .store(RecoveryStep::BeginQuiesce as u8, Ordering::Release);
                        continue;
                    };
                    let drains = self.recovery_irq_drains.lock();
                    let mut complete = true;
                    for (index, registration) in registrations.iter().enumerate() {
                        let Some(token) = drains[index] else {
                            complete = false;
                            break;
                        };
                        match registration.action_drain_complete(token) {
                            Ok(drained) => complete &= drained,
                            Err(error) => {
                                drop(drains);
                                drop(registration_guard);
                                self.mark_offline();
                                self.recovery_step
                                    .store(RecoveryStep::Finished as u8, Ordering::Release);
                                error!(
                                    "block controller {} lost IRQ drain ownership: {error:?}",
                                    self.name
                                );
                                return WorkOutcome::Complete;
                            }
                        }
                    }
                    if !complete {
                        // The last target-action guard queues this work item.
                        // Unrelated actions on the shared descriptor are not
                        // part of the wait and cannot force a polling loop.
                        return WorkOutcome::Complete;
                    }
                    self.recovery_step
                        .store(RecoveryStep::BeginQuiesce as u8, Ordering::Release);
                }
                RecoveryStep::BeginQuiesce => {
                    if self.recovery_failed.load(Ordering::Acquire) {
                        self.mark_offline();
                        self.recovery_step
                            .store(RecoveryStep::Finished as u8, Ordering::Release);
                        return WorkOutcome::Complete;
                    }
                    let epoch = self.expected_recovery_epoch();
                    let Some(cause) = *self.recovery_cause.lock() else {
                        self.finish_failed_recovery_init(
                            "lost the first recovery cause",
                            InitError::InvalidState,
                        );
                        return WorkOutcome::Complete;
                    };
                    let result = match self.device.lock().bundle_mut().lifecycle() {
                        LifecycleEndpoint::Inline => Err(InitError::InvalidState),
                        LifecycleEndpoint::Interrupt(lifecycle) => {
                            if lifecycle.controller_cookie() != self.lifecycle_cookie {
                                Err(InitError::InvalidState)
                            } else {
                                lifecycle.begin_dma_quiesce(epoch, cause)
                            }
                        }
                    };
                    if let Err(error) = result {
                        self.finish_failed_recovery_init("could not begin DMA quiescence", error);
                        return WorkOutcome::Complete;
                    }
                    self.recovery_step
                        .store(RecoveryStep::PollQuiesce as u8, Ordering::Release);
                }
                RecoveryStep::PollQuiesce => {
                    let input = self.recovery_input(false);
                    let progress = match self.device.lock().bundle_mut().lifecycle() {
                        LifecycleEndpoint::Inline => InitPoll::Failed(InitError::InvalidState),
                        LifecycleEndpoint::Interrupt(lifecycle) => {
                            lifecycle.poll_dma_quiesce(input)
                        }
                    };
                    match progress {
                        InitPoll::Ready(proof) => {
                            self.finish_recovery_poll();
                            if let Err(error) = self.validate_dma_proof(&proof) {
                                self.finish_failed_recovery_init(
                                    "returned an invalid DMA proof",
                                    error,
                                );
                                return WorkOutcome::Complete;
                            }
                            for queue in self.runtime_queues() {
                                if let RuntimeQueue::Interrupt(queue) = queue
                                    && let Err(error) = queue.reclaim_after_quiesce(&proof)
                                {
                                    if queue.is_fatal_completion_quarantined() {
                                        error!(
                                            "block controller {} isolated a fatal completion \
                                             quarantine after IRQ/DMA drain: {error}",
                                            self.name
                                        );
                                        self.mark_offline();
                                        self.recovery_step
                                            .store(RecoveryStep::Finished as u8, Ordering::Release);
                                        return WorkOutcome::Complete;
                                    }
                                    self.finish_failed_recovery_hctx(
                                        "could not reclaim accepted requests",
                                        error,
                                    );
                                    return WorkOutcome::Complete;
                                }
                            }
                            for queue in self.runtime_queues() {
                                if let RuntimeQueue::Interrupt(queue) = queue
                                    && let Err(error) = queue.begin_reinitialization()
                                {
                                    self.finish_failed_recovery_hctx(
                                        "could not enter queue reinitialization",
                                        error,
                                    );
                                    return WorkOutcome::Complete;
                                }
                            }
                            let result = match self.device.lock().bundle_mut().lifecycle() {
                                LifecycleEndpoint::Inline => Err(InitError::InvalidState),
                                LifecycleEndpoint::Interrupt(lifecycle) => {
                                    lifecycle.begin_reinitialize(proof)
                                }
                            };
                            if let Err(error) = result {
                                self.finish_failed_recovery_init(
                                    "could not begin controller reinitialization",
                                    error,
                                );
                                return WorkOutcome::Complete;
                            }
                            self.recovery_step
                                .store(RecoveryStep::PollReinitialize as u8, Ordering::Release);
                        }
                        InitPoll::Pending(schedule) => {
                            if let Err(error) = self.arm_recovery_schedule(schedule, false) {
                                self.finish_failed_recovery_init(
                                    "published an invalid DMA-quiesce schedule",
                                    error,
                                );
                            }
                            return WorkOutcome::Complete;
                        }
                        InitPoll::Failed(error) => {
                            self.finish_recovery_poll();
                            self.finish_failed_recovery_init(
                                "could not prove DMA quiescence",
                                error,
                            );
                            return WorkOutcome::Complete;
                        }
                    }
                }
                RecoveryStep::PollReinitialize => {
                    let input = self.recovery_input(true);
                    let progress = match self.device.lock().bundle_mut().lifecycle() {
                        LifecycleEndpoint::Inline => InitPoll::Failed(InitError::InvalidState),
                        LifecycleEndpoint::Interrupt(lifecycle) => {
                            lifecycle.poll_reinitialize(input)
                        }
                    };
                    match progress {
                        InitPoll::Ready(proof) => {
                            self.finish_recovery_poll();
                            if let Err(error) = self.validate_ready_proof(&proof) {
                                self.finish_failed_recovery_init(
                                    "returned an invalid ready proof",
                                    error,
                                );
                                return WorkOutcome::Complete;
                            }
                            self.recovery_step
                                .store(RecoveryStep::EnableActions as u8, Ordering::Release);
                        }
                        InitPoll::Pending(schedule) => {
                            if let Err(error) = self.arm_recovery_schedule(schedule, true) {
                                self.finish_failed_recovery_init(
                                    "published an invalid reinitialization schedule",
                                    error,
                                );
                            }
                            return WorkOutcome::Complete;
                        }
                        InitPoll::Failed(error) => {
                            self.finish_recovery_poll();
                            self.finish_failed_recovery_init(
                                "controller reinitialization failed",
                                error,
                            );
                            return WorkOutcome::Complete;
                        }
                    }
                }
                RecoveryStep::EnableActions => {
                    if let Err(error) = self.enable_recovery_irqs() {
                        self.finish_failed_recovery_init("could not restore IRQ delivery", error);
                        return WorkOutcome::Complete;
                    }
                    for queue in self.runtime_queues() {
                        if let RuntimeQueue::Interrupt(queue) = queue
                            && let Err(error) = queue.finish_reinitialization()
                        {
                            self.finish_failed_recovery_hctx(
                                "could not publish a reinitialized queue",
                                error,
                            );
                            return WorkOutcome::Complete;
                        }
                    }
                    self.recovery_wait_sources.store(0, Ordering::Release);
                    self.recovery_pending_sources.store(0, Ordering::Release);
                    self.recovery_polling_irqs.store(false, Ordering::Release);
                    self.recovery_irq_drains.lock().fill(None);
                    *self.recovery_cause.lock() = None;
                    if self
                        .phase
                        .compare_exchange(
                            ControllerPhase::Recovering as u8,
                            ControllerPhase::Running as u8,
                            Ordering::AcqRel,
                            Ordering::Acquire,
                        )
                        .is_err()
                    {
                        // A synchronous return waiter can quarantine this
                        // controller while the recovery callback is finishing.
                        // A late callback must not reopen the controller or
                        // leave its just-enabled routes live.
                        if !self.mask_recovery_sources() {
                            error!(
                                "block controller {} retained live IRQ actions after a late \
                                 recovery callback",
                                self.name
                            );
                        }
                        self.mark_offline();
                    }
                    self.recovery_step
                        .store(RecoveryStep::Finished as u8, Ordering::Release);
                    self.operation_wait.notify_all();
                    return WorkOutcome::Complete;
                }
                RecoveryStep::Idle | RecoveryStep::Finished => return WorkOutcome::Complete,
            }
        }

        let _ = self.queue_recovery_work();
        WorkOutcome::Complete
    }

    fn finish_failed_recovery_init(&'static self, operation: &str, error: InitError) {
        error!("block controller {} {operation}: {error}", self.name);
        self.defer_failed_recovery();
    }

    fn finish_failed_recovery_hctx(&'static self, operation: &str, error: HardwareQueueError) {
        error!("block controller {} {operation}: {error}", self.name);
        self.defer_failed_recovery();
    }

    fn defer_failed_recovery(&'static self) {
        self.recovery_failed.store(true, Ordering::Release);
        self.recovery_step
            .store(RecoveryStep::DisableActions as u8, Ordering::Release);
        let _ = self.queue_recovery_work();
    }

    pub(super) fn abort_failed_activation(self: &Arc<Self>) {
        self.phase
            .store(ControllerPhase::Offline as u8, Ordering::Release);
        if !self.mask_recovery_sources() {
            // No owner outside this activation retains the unpublished
            // controller. Leak one Arc intentionally so its live IRQ actions,
            // handler targets, mappings, and DMA storage survive an unproven
            // device mask for shutdown lifetime.
            core::mem::forget(Arc::clone(self));
            return;
        }
        if let Some(registrations) = self.registrations.lock().as_ref() {
            for registration in registrations {
                let _ = registration.synchronize();
            }
        }

        let controller: &'static Self = unsafe {
            // SAFETY: this Arc keeps the allocation alive until recovery work
            // and every hctx work item have been synchronously drained below.
            &*Arc::as_ptr(self)
        };
        let _ = controller
            .recovery_domain
            .cancel_work_sync(controller.recovery_work());
        rollback_unpublished_runtime_devices(&self.devices);
        self.owner_link.clear_after_drain(self);
    }
}
