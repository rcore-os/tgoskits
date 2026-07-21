//! CPU-local block IRQ capture and maintenance-owner routing.

mod ledger;
mod quarantine;

use alloc::{boxed::Box, format, sync::Arc};
use core::{
    marker::PhantomData,
    sync::atomic::{AtomicU64, Ordering},
};

use ledger::IrqSourceLedger;
pub(in crate::block) use ledger::RuntimeIrqSourceError;
use quarantine::IrqSourceQuarantineReservation;
use rdif_block::{
    BIrqControl, BlkError, BlockIrqSource, ContainmentCause, Event, FaultContainment, IrqCapture,
    IrqControlError, MaskedSource,
};

use crate::{
    block::hctx_model::{HctxIrqPublication, HctxTerminalGate, HctxTerminalPermit},
    maintenance::{
        LocalIrqWake, LocalIrqWakeError, MaintenanceCauses, MaintenanceDetachedIrqAction,
        MaintenanceError, MaintenanceIrqAction, MaintenancePublishResult, MaintenanceRegistrar,
        MaintenanceSession,
    },
};

/// Immutable fact transferred from one device top half to its sole owner.
#[derive(Clone, Copy, Debug)]
pub(in crate::block) enum BlockMaintenanceEvent {
    /// Device status was acknowledged into a stable block event.
    Irq {
        source_id: usize,
        source_epoch: u64,
        facts: Event,
        masked: Option<MaskedSource>,
    },
    /// Capture failed after classifying device-source containment.
    Fault {
        source_id: usize,
        reason: BlkError,
        containment: FaultContainment,
    },
}

/// IRQ sources whose event could not fit in the fixed maintenance mailbox.
///
/// The top half sets one bit before returning a contained action result. The
/// owner consumes the bits and enters recovery; no completion fact is inferred
/// from this side channel.
pub(in crate::block) struct BlockIrqFaultSet {
    publication_failures: AtomicU64,
    terminal_gate: HctxTerminalGate,
}

impl BlockIrqFaultSet {
    pub(in crate::block) const fn new() -> Self {
        Self {
            publication_failures: AtomicU64::new(0),
            terminal_gate: HctxTerminalGate::new(),
        }
    }

    /// Orders hard-IRQ capture and mailbox publication before watchdog claim.
    fn begin_irq_publication(&self) -> Option<HctxIrqPublication<'_>> {
        self.terminal_gate.begin_irq_publication()
    }

    /// Establishes a controller-wide watchdog cutoff after earlier IRQ ingress.
    pub(in crate::block) fn try_begin_watchdog_cutoff(&self) -> Option<HctxTerminalPermit<'_>> {
        self.terminal_gate.try_begin_terminal()
    }

    fn record(&self, source_id: usize) {
        if source_id < u64::BITS as usize {
            self.publication_failures
                .fetch_or(1_u64 << source_id, Ordering::Release);
        }
    }

    pub(in crate::block) fn take(&self) -> rdif_block::IdList {
        rdif_block::IdList::from_bits(self.publication_failures.swap(0, Ordering::AcqRel))
    }
}

/// Owner-side registration and rearm capability for one block IRQ source.
pub(in crate::block) struct RuntimeIrqSource {
    source_id: usize,
    ledger: IrqSourceLedger,
    control: Option<BIrqControl>,
    action: Option<RuntimeIrqAction>,
    quarantine: Option<IrqSourceQuarantineReservation>,
    _not_send: PhantomData<*mut ()>,
}

pub(in crate::block) fn runtime_irq_source_mut(
    sources: &mut [RuntimeIrqSource],
    source_id: usize,
) -> Result<&mut RuntimeIrqSource, RuntimeIrqSourceError> {
    sources
        .iter_mut()
        .find(|source| source.source_id == source_id)
        .ok_or(RuntimeIrqSourceError::UnknownSource { source_id })
}

/// Failed consuming close that retains the complete owner-local IRQ source.
pub(in crate::block) struct RuntimeIrqSourceCloseFailure {
    reason: MaintenanceError,
    source: Box<RuntimeIrqSource>,
}

enum RuntimeIrqAction {
    Active(MaintenanceIrqAction),
    Detached(MaintenanceDetachedIrqAction),
}

pub(in crate::block) struct RuntimeIrqRegistration {
    pub(in crate::block) controller_name: alloc::string::String,
    pub(in crate::block) source_id: usize,
    pub(in crate::block) irq: ax_hal::irq::IrqId,
    pub(in crate::block) source: BlockIrqSource,
    pub(in crate::block) wake: LocalIrqWake<BlockMaintenanceEvent>,
    pub(in crate::block) faults: Arc<BlockIrqFaultSet>,
}

trait RuntimeIrqActionRegistrar {
    fn owner_cpu(&self) -> usize;

    fn register_shared_disabled<H>(
        &self,
        name: alloc::string::String,
        irq: ax_hal::irq::IrqId,
        handler: H,
    ) -> Result<MaintenanceIrqAction, MaintenanceError>
    where
        H: FnMut(ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn + Send + 'static;
}

impl RuntimeIrqActionRegistrar for MaintenanceRegistrar<BlockMaintenanceEvent> {
    fn owner_cpu(&self) -> usize {
        self.owner_cpu()
    }

    fn register_shared_disabled<H>(
        &self,
        name: alloc::string::String,
        irq: ax_hal::irq::IrqId,
        handler: H,
    ) -> Result<MaintenanceIrqAction, MaintenanceError>
    where
        H: FnMut(ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn + Send + 'static,
    {
        MaintenanceRegistrar::register_shared_disabled(self, name, irq, handler)
    }
}

impl RuntimeIrqActionRegistrar for MaintenanceSession<BlockMaintenanceEvent> {
    fn owner_cpu(&self) -> usize {
        self.owner_cpu()
    }

    fn register_shared_disabled<H>(
        &self,
        name: alloc::string::String,
        irq: ax_hal::irq::IrqId,
        handler: H,
    ) -> Result<MaintenanceIrqAction, MaintenanceError>
    where
        H: FnMut(ax_hal::irq::IrqContext) -> ax_hal::irq::IrqReturn + Send + 'static,
    {
        MaintenanceSession::register_shared_disabled(self, name, irq, handler)
    }
}

impl RuntimeIrqSource {
    fn register_disabled_with<R: RuntimeIrqActionRegistrar>(
        registrar: &R,
        request: RuntimeIrqRegistration,
    ) -> Result<Self, MaintenanceError> {
        let RuntimeIrqRegistration {
            controller_name,
            source_id,
            irq,
            source,
            wake,
            faults,
        } = request;
        if source_id >= u64::BITS as usize {
            return Err(ax_hal::irq::IrqError::InvalidIrq.into());
        }
        let quarantine = IrqSourceQuarantineReservation::reserve()?;
        let (mut endpoint, control) = source.into_parts();
        let action_name = format!("{controller_name}/blk-source-{source_id}");
        let mut source_epoch = 0_u64;
        let owner_cpu = registrar.owner_cpu();
        let registration =
            registrar.register_shared_disabled(action_name, irq, move |context| {
                if context.cpu.0 != owner_cpu {
                    faults.record(source_id);
                    return contain_or_mask_line(
                        endpoint.as_mut(),
                        ContainmentCause::OwnerUnavailable,
                    );
                }
                // This is the first software linearization point after the
                // device raised the IRQ. A watchdog cutoff that observes this
                // publisher must defer until the stable mailbox fact exists.
                // A callback that observes an existing cutoff is ordered late
                // and may still publish for recovery processing.
                let _publication = faults.begin_irq_publication();
                match endpoint.capture() {
                    IrqCapture::Unhandled => ax_hal::irq::IrqReturn::Unhandled,
                    IrqCapture::Captured { event, masked } => {
                        source_epoch = next_nonzero_epoch(source_epoch);
                        let event = BlockMaintenanceEvent::Irq {
                            source_id,
                            source_epoch,
                            facts: event,
                            masked,
                        };
                        match wake.publish_from_irq(MaintenanceCauses::IRQ, event) {
                            Ok(MaintenancePublishResult::Published) => ax_hal::irq::IrqReturn::Wake,
                            Ok(MaintenancePublishResult::Overflowed) => {
                                faults.record(source_id);
                                contain_after_publication_failure(
                                    endpoint.as_mut(),
                                    ContainmentCause::PublicationFull,
                                )
                            }
                            Err(error) => {
                                faults.record(source_id);
                                contain_after_publication_failure(
                                    endpoint.as_mut(),
                                    containment_cause_for_wake_error(error),
                                )
                            }
                        }
                    }
                    IrqCapture::Fault {
                        reason,
                        containment,
                    } => {
                        let event = BlockMaintenanceEvent::Fault {
                            source_id,
                            reason,
                            containment,
                        };
                        let publication = wake.publish_from_irq(MaintenanceCauses::IRQ, event);
                        match (containment, publication) {
                            (
                                FaultContainment::DeviceSourceMasked(_),
                                Ok(MaintenancePublishResult::Published),
                            ) => ax_hal::irq::IrqReturn::DisableActionAndWake,
                            (FaultContainment::DeviceSourceMasked(_), _) => {
                                faults.record(source_id);
                                ax_hal::irq::IrqReturn::DisableActionAndWake
                            }
                            (FaultContainment::Uncontained, _) => {
                                faults.record(source_id);
                                contain_after_publication_failure(
                                    endpoint.as_mut(),
                                    ContainmentCause::CaptureFault,
                                )
                            }
                        }
                    }
                }
            })?;
        Ok(Self {
            source_id,
            ledger: IrqSourceLedger::default(),
            control: Some(control),
            action: Some(RuntimeIrqAction::Active(registration)),
            quarantine: Some(quarantine.bind(source_id)),
            _not_send: PhantomData,
        })
    }

    pub(in crate::block) fn register_initial_disabled(
        registrar: &MaintenanceRegistrar<BlockMaintenanceEvent>,
        request: RuntimeIrqRegistration,
    ) -> Result<Self, MaintenanceError> {
        Self::register_disabled_with(registrar, request)
    }

    pub(in crate::block) fn register_replacement_disabled(
        session: &MaintenanceSession<BlockMaintenanceEvent>,
        request: RuntimeIrqRegistration,
    ) -> Result<Self, MaintenanceError> {
        Self::register_disabled_with(session, request)
    }

    pub(in crate::block) const fn source_id(&self) -> usize {
        self.source_id
    }

    pub(in crate::block) fn enable(&self) -> Result<(), MaintenanceError> {
        self.active_action()?.enable()
    }

    pub(in crate::block) fn disable(&self) -> Result<(), MaintenanceError> {
        self.active_action()?.disable()
    }

    pub(in crate::block) fn synchronize(&self) -> Result<(), MaintenanceError> {
        self.active_action()?.synchronize()
    }

    pub(in crate::block) fn status(&self) -> Result<ax_hal::irq::IrqStatus, MaintenanceError> {
        self.active_action()?.status()
    }

    pub(in crate::block) fn device_state(&self) -> Option<rdif_block::IrqSourceState> {
        self.control.as_ref()?.state()
    }

    /// Releases this action's fail-closed line quench after the owner has
    /// successfully masked the corresponding device source.
    pub(in crate::block) fn release_quench(&self) -> Result<(), MaintenanceError> {
        self.active_action()?.release_quench()
    }

    /// Records one stable IRQ fact before it is routed to initialization or a
    /// runtime queue. The source mask becomes owned by this registration.
    pub(in crate::block) fn record_service_fact(
        &mut self,
        masked: Option<MaskedSource>,
    ) -> Result<(), RuntimeIrqSourceError> {
        self.ledger.record_service_fact(self.source_id, masked)
    }

    /// Marks every fact currently routed from this source as serviced.
    pub(in crate::block) fn finish_service(&mut self) {
        self.ledger.finish_service();
    }

    /// Rearms the exact retained mask only after its captured facts drain.
    ///
    /// A failed control operation leaves the token in `self.ledger`, so
    /// recovery can diagnose or retire the same generation without guessing.
    pub(in crate::block) fn rearm_retained(&mut self) -> Result<bool, RuntimeIrqSourceError> {
        if self.ledger.service_pending() {
            return Err(RuntimeIrqSourceError::ServicePending {
                source_id: self.source_id,
            });
        }

        // Reopen the OS action while the generation-bearing device source is
        // still masked. If the action cannot be enabled, hardware remains
        // contained. A failed device rearm is rolled back to a disabled action.
        let irq_guard = ax_kspin::IrqGuard::new();
        let source_id = self.source_id;
        let action = self.action.as_ref();
        let control = self.control.as_mut();
        let result = self.ledger.try_rearm(|masked| {
            let operation = (|| {
                match action {
                    Some(RuntimeIrqAction::Active(action)) => {
                        action.enable().map_err(|_| IrqControlError::Offline)?
                    }
                    Some(RuntimeIrqAction::Detached(_)) | None => {
                        return Err(IrqControlError::Offline);
                    }
                }
                match control {
                    Some(control) => control.rearm(masked),
                    None => Err(IrqControlError::Offline),
                }
            })();
            if operation.is_err()
                && let Some(RuntimeIrqAction::Active(action)) = action
            {
                let _ = action.disable();
            }
            operation.map_err(|error| RuntimeIrqSourceError::Rearm {
                source_id,
                generation: masked.lifecycle_generation().get(),
                mask_epoch: masked.mask_epoch().get(),
                bitmap: masked.bitmap().get(),
                error,
            })
        });
        drop(irq_guard);
        result
    }

    /// Retires every old mask only after reset or full device-source masking
    /// has made the old generation unreachable by hardware.
    pub(in crate::block) fn discard_ledger_after_device_quiesce(&mut self) {
        self.ledger.discard_after_device_quiesce();
    }

    pub(in crate::block) fn detach(&mut self) -> Result<(), MaintenanceError> {
        let action = self
            .action
            .take()
            .ok_or(MaintenanceError::Irq(ax_hal::irq::IrqError::NotFound))?;
        match action {
            RuntimeIrqAction::Active(action) => match action.detach() {
                Ok(detached) => {
                    self.action = Some(RuntimeIrqAction::Detached(detached));
                    Ok(())
                }
                Err(failure) => {
                    let (reason, action) = failure.into_parts();
                    self.action = Some(RuntimeIrqAction::Active(action));
                    Err(reason)
                }
            },
            RuntimeIrqAction::Detached(detached) => {
                self.action = Some(RuntimeIrqAction::Detached(detached));
                Ok(())
            }
        }
    }

    pub(in crate::block) fn reattach(&mut self) -> Result<(), MaintenanceError> {
        let action = self
            .action
            .take()
            .ok_or(MaintenanceError::Irq(ax_hal::irq::IrqError::NotFound))?;
        match action {
            RuntimeIrqAction::Active(registration) => {
                self.action = Some(RuntimeIrqAction::Active(registration));
                Ok(())
            }
            RuntimeIrqAction::Detached(detached) => match detached.reattach() {
                Ok(action) => {
                    self.action = Some(RuntimeIrqAction::Active(action));
                    Ok(())
                }
                Err(failure) => {
                    let (reason, action) = failure.into_parts();
                    self.action = Some(RuntimeIrqAction::Detached(action));
                    Err(reason)
                }
            },
        }
    }

    fn active_action(&self) -> Result<&MaintenanceIrqAction, MaintenanceError> {
        match self.action.as_ref() {
            Some(RuntimeIrqAction::Active(action)) => Ok(action),
            Some(RuntimeIrqAction::Detached(_)) | None => {
                Err(MaintenanceError::Irq(ax_hal::irq::IrqError::NotFound))
            }
        }
    }

    pub(in crate::block) fn close(mut self) -> Result<(), RuntimeIrqSourceCloseFailure> {
        let action = self
            .action
            .take()
            .expect("live block IRQ source retains exactly one action");
        let control = self
            .control
            .take()
            .expect("live block IRQ source retains exactly one control endpoint");
        let reservation = self
            .quarantine
            .take()
            .expect("live block IRQ source retains one quarantine reservation");

        match action {
            RuntimeIrqAction::Active(action) => match action.close() {
                Ok(()) => {
                    drop(control);
                    reservation.release();
                    Ok(())
                }
                Err(failure) => {
                    let (reason, action) = failure.into_parts();
                    self.action = Some(RuntimeIrqAction::Active(action));
                    self.control = Some(control);
                    self.quarantine = Some(reservation);
                    Err(RuntimeIrqSourceCloseFailure {
                        reason,
                        source: Box::new(self),
                    })
                }
            },
            RuntimeIrqAction::Detached(action) => match action.close() {
                Ok(()) => {
                    drop(control);
                    reservation.release();
                    Ok(())
                }
                Err(failure) => {
                    let (reason, action) = failure.into_parts();
                    self.action = Some(RuntimeIrqAction::Detached(action));
                    self.control = Some(control);
                    self.quarantine = Some(reservation);
                    Err(RuntimeIrqSourceCloseFailure {
                        reason,
                        source: Box::new(self),
                    })
                }
            },
        }
    }
}

impl RuntimeIrqSourceCloseFailure {
    pub(in crate::block) fn into_parts(self) -> (MaintenanceError, RuntimeIrqSource) {
        (self.reason, *self.source)
    }
}

/// Quiesces actions only after the caller has successfully masked the device.
///
/// Disabling every action first prevents new owner callbacks. Releasing each
/// action-owned line quench then lets unrelated shared-line peers progress,
/// while the final synchronize proves all callbacks for this owner are gone.
pub(in crate::block) fn quiesce_after_device_masked(
    sources: &[RuntimeIrqSource],
) -> Result<(), MaintenanceError> {
    for source in sources {
        source.disable()?;
    }
    for source in sources {
        source.release_quench()?;
    }
    for source in sources {
        source.synchronize()?;
    }
    Ok(())
}

impl Drop for RuntimeIrqSource {
    fn drop(&mut self) {
        let Some(action) = self.action.take() else {
            return;
        };
        let control = self
            .control
            .take()
            .expect("live block IRQ source retains its owner control");
        let reservation = self
            .quarantine
            .take()
            .expect("live block IRQ source retains quarantine capacity");
        // Active typed actions fall into Registration's fixed quarantine,
        // retaining the callback, endpoint, and LocalIrqWake. Detached actions
        // are already outside the descriptor and may destroy their callback.
        // The block registry separately retains the control capability so an
        // unexpected Drop never turns into anonymous ownership loss.
        drop(action);
        reservation.retain(self.source_id, control);
    }
}

fn next_nonzero_epoch(epoch: u64) -> u64 {
    let next = epoch.wrapping_add(1);
    if next == 0 { 1 } else { next }
}

fn containment_cause_for_wake_error(error: LocalIrqWakeError) -> ContainmentCause {
    match error {
        LocalIrqWakeError::Closed => ContainmentCause::PublicationClosed,
        LocalIrqWakeError::NotHardIrq
        | LocalIrqWakeError::WrongCpu { .. }
        | LocalIrqWakeError::OwnerIdentityMismatch
        | LocalIrqWakeError::OwnerPlacementMismatch { .. }
        | LocalIrqWakeError::OwnerUnavailable { .. } => ContainmentCause::OwnerUnavailable,
    }
}

fn contain_after_publication_failure(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = Event, Fault = BlkError>,
    cause: ContainmentCause,
) -> ax_hal::irq::IrqReturn {
    match endpoint.contain(cause) {
        Ok(_) => ax_hal::irq::IrqReturn::DisableActionAndWake,
        Err(_) => ax_hal::irq::IrqReturn::MaskLineAndWake,
    }
}

fn contain_or_mask_line(
    endpoint: &mut dyn rdif_block::IrqEndpoint<Event = Event, Fault = BlkError>,
    cause: ContainmentCause,
) -> ax_hal::irq::IrqReturn {
    contain_after_publication_failure(endpoint, cause)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULT_ERROR_SIZE_BUDGET: usize = 128;

    #[test]
    fn runtime_irq_source_close_failure_fits_the_result_error_budget() {
        assert!(
            core::mem::size_of::<RuntimeIrqSourceCloseFailure>() <= RESULT_ERROR_SIZE_BUDGET,
            "IRQ-source close failure must stay compact while retaining the complete owner"
        );
    }

    #[test]
    fn captured_irq_publication_precedes_watchdog_cutoff() {
        let ingress = BlockIrqFaultSet::new();
        let publication = ingress
            .begin_irq_publication()
            .expect("an open IRQ ingress accepts one publisher");

        assert!(
            ingress.try_begin_watchdog_cutoff().is_none(),
            "watchdog must not pass an IRQ already publishing to the owner mailbox"
        );

        drop(publication);
        assert!(ingress.try_begin_watchdog_cutoff().is_some());
    }

    #[test]
    fn failed_rearm_retains_the_exact_mask_capability() {
        let token = MaskedSource::try_new(7, 0b101).unwrap();
        let mut ledger = IrqSourceLedger::default();
        ledger.record_service_fact(3, Some(token)).unwrap();
        ledger.finish_service();

        let result = ledger.try_rearm(|observed| {
            assert_eq!(observed, token);
            Err::<(), _>(IrqControlError::Offline)
        });

        assert_eq!(result, Err(IrqControlError::Offline));
        assert_eq!(ledger.retained_mask(), Some(token));
    }

    #[test]
    fn same_generation_masks_merge_but_new_generation_is_rejected() {
        let mut ledger = IrqSourceLedger::default();
        ledger
            .record_service_fact(5, Some(MaskedSource::try_new(11, 0b001).unwrap()))
            .unwrap();
        ledger
            .record_service_fact(5, Some(MaskedSource::try_new(11, 0b100).unwrap()))
            .unwrap();
        assert_eq!(
            ledger.retained_mask(),
            Some(MaskedSource::try_new(11, 0b101).unwrap())
        );

        assert_eq!(
            ledger.record_service_fact(5, Some(MaskedSource::try_new(12, 0b010).unwrap())),
            Err(RuntimeIrqSourceError::ConflictingGeneration {
                source_id: 5,
                retained_generation: 11,
                observed_generation: 12,
            })
        );
        assert_eq!(
            ledger.retained_mask(),
            Some(MaskedSource::try_new(11, 0b101).unwrap())
        );
    }

    #[test]
    fn a_new_mask_epoch_cannot_merge_with_retained_evidence() {
        let mut ledger = IrqSourceLedger::default();
        let retained = MaskedSource::try_new_with_epoch(7, 31, 0b001).unwrap();
        let newer = MaskedSource::try_new_with_epoch(7, 32, 0b010).unwrap();
        ledger.record_service_fact(2, Some(retained)).unwrap();

        assert!(matches!(
            ledger.record_service_fact(2, Some(newer)),
            Err(RuntimeIrqSourceError::ConflictingMaskEpoch {
                source_id: 2,
                retained_mask_epoch: 31,
                observed_mask_epoch: 32,
            })
        ));
        assert_eq!(ledger.retained_mask(), Some(retained));
    }
}
