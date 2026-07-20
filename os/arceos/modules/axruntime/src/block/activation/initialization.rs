//! IRQ-bound controller initialization and unpublished-owner rollback.

use alloc::{boxed::Box, sync::Arc, vec::Vec};

use ax_driver::block::RdifBlockDevice;
use rdif_block::{ControllerInitEndpoint, IdList, InitError, InitInput, InitPoll, MaskedSource};

use super::{
    BlockControllerError, ControllerInitialization, close_irq_sources, finish_maintenance_close,
    quarantine_source_close_failure, with_owner_irq_excluded,
};
use crate::{
    block::controller::source::{
        BlockIrqFaultSet, BlockMaintenanceEvent, RuntimeIrqRegistration, RuntimeIrqSource,
        quiesce_after_device_masked,
    },
    maintenance::{
        MaintenanceCauses, MaintenanceClosed, MaintenanceError, MaintenanceRegistrar,
        MaintenanceSession, MaintenanceWaitOutcome,
    },
    task::yield_current_cpu,
};

const INIT_TRANSITION_BUDGET: usize = 64;

pub(super) fn register_initial_sources(
    device: &mut RdifBlockDevice,
    registrar: &MaintenanceRegistrar<BlockMaintenanceEvent>,
    declared: IdList,
    faults: Arc<BlockIrqFaultSet>,
) -> Result<Vec<RuntimeIrqSource>, SourceRegistrationFailure> {
    let mut sources = Vec::new();
    for source_id in declared.iter() {
        let registration = (|| -> Result<RuntimeIrqSource, BlockControllerError> {
            let binding = device
                .irq_for_source(source_id)
                .cloned()
                .ok_or(BlockControllerError::MissingIrqBinding(source_id))?;
            let irq = crate::irq::resolve_binding_irq(binding)?;
            let source = match device.bundle_mut().controller_init() {
                ControllerInitEndpoint::Pending(initializer) => initializer
                    .take_irq_source(source_id)
                    .ok_or(BlockControllerError::MissingIrqHandler(source_id))?,
                ControllerInitEndpoint::Ready => {
                    return Err(BlockControllerError::Initialization(
                        InitError::InvalidState,
                    ));
                }
            };
            let wake = registrar.local_irq_wake()?;
            RuntimeIrqSource::register_initial_disabled(
                registrar,
                RuntimeIrqRegistration {
                    controller_name: device.name().into(),
                    source_id,
                    irq,
                    source,
                    wake,
                    faults: Arc::clone(&faults),
                },
            )
            .map_err(BlockControllerError::from)
        })();
        match registration {
            Ok(source) => sources.push(source),
            Err(error) => {
                return Err(SourceRegistrationFailure {
                    error: Box::new(error),
                    sources,
                });
            }
        }
    }
    Ok(sources)
}

pub(super) struct SourceRegistrationFailure {
    error: Box<BlockControllerError>,
    sources: Vec<RuntimeIrqSource>,
}

impl SourceRegistrationFailure {
    pub(super) fn into_parts(self) -> (BlockControllerError, Vec<RuntimeIrqSource>) {
        (*self.error, self.sources)
    }
}

pub(super) fn enable_irq_delivery(
    device: &RdifBlockDevice,
    sources: &[RuntimeIrqSource],
) -> Result<(), BlockControllerError> {
    if sources.is_empty() {
        return Ok(());
    }
    let mut enabled = 0;
    for source in sources {
        if let Err(error) = source.enable() {
            for rollback in &sources[..enabled] {
                let _ = rollback.disable();
            }
            return Err(error.into());
        }
        enabled += 1;
    }
    if let Err(error) = with_owner_irq_excluded(|| device.enable_irq()) {
        let _ = with_owner_irq_excluded(|| device.disable_irq());
        for source in &sources[..enabled] {
            let _ = source.disable();
        }
        return Err(error.into());
    }
    Ok(())
}

pub(super) fn drive_init_fsm(
    device: &mut RdifBlockDevice,
    session: &MaintenanceSession<BlockMaintenanceEvent>,
    sources: &mut [RuntimeIrqSource],
    faults: &BlockIrqFaultSet,
    pending: &mut IdList,
    masked: &mut [Option<MaskedSource>; 64],
) -> Result<(), InitError> {
    let mut transitions = 0;
    loop {
        drain_init_events(session, faults, pending, masked)?;
        let input_sources = *pending;
        *pending = IdList::none();
        let progress = with_owner_irq_excluded(|| match device.bundle_mut().controller_init() {
            ControllerInitEndpoint::Ready => InitPoll::Ready(()),
            ControllerInitEndpoint::Pending(initializer) => initializer.poll_init(InitInput::new(
                ax_hal::time::monotonic_time_nanos(),
                input_sources,
            )),
        });
        rearm_consumed_sources(sources, input_sources, masked)?;

        match progress {
            InitPoll::Ready(()) => return Ok(()),
            InitPoll::Failed(error) => return Err(error),
            InitPoll::Pending(schedule) => {
                let schedule = schedule.validate()?;
                transitions += 1;
                if schedule.run_again() {
                    if transitions == INIT_TRANSITION_BUDGET {
                        transitions = 0;
                        yield_current_cpu().map_err(|_| {
                            InitError::Hardware("maintenance owner could not yield init budget")
                        })?;
                    }
                    continue;
                }
                transitions = 0;
                let outcome = if let Some(deadline) = schedule.wake_at_ns() {
                    session.wait_for_pending_until(deadline).map_err(|_| {
                        InitError::Hardware("maintenance owner could not wait for init deadline")
                    })?
                } else {
                    session.wait_for_pending().map_err(|_| {
                        InitError::Hardware("maintenance owner could not wait for init IRQ")
                    })?;
                    MaintenanceWaitOutcome::ConditionMet
                };
                let _deadline_elapsed = matches!(outcome, MaintenanceWaitOutcome::TimedOut);
            }
        }
    }
}

fn drain_init_events(
    session: &MaintenanceSession<BlockMaintenanceEvent>,
    faults: &BlockIrqFaultSet,
    pending: &mut IdList,
    masked: &mut [Option<MaskedSource>; 64],
) -> Result<(), InitError> {
    if !faults.take().is_empty() {
        return Err(InitError::Hardware("initialization IRQ publication failed"));
    }
    let mut fault = None;
    let drain = session
        .drain_owner(
            crate::maintenance::MAINTENANCE_BATCH_LIMIT,
            |event| match event {
                BlockMaintenanceEvent::Irq {
                    source_id,
                    source_epoch: _,
                    facts: _,
                    masked: token,
                } => {
                    pending.insert(source_id);
                    if source_id < masked.len() && token.is_some() {
                        masked[source_id] = token;
                    }
                }
                BlockMaintenanceEvent::Fault {
                    source_id, reason, ..
                } => {
                    pending.insert(source_id);
                    fault = Some(match reason {
                        rdif_block::BlkError::TimedOut => InitError::TimedOut,
                        _ => InitError::Hardware("initialization IRQ capture failed"),
                    });
                }
            },
        )
        .map_err(|_| InitError::Hardware("initialization IRQ mailbox drain failed"))?;
    if drain.causes().contains(MaintenanceCauses::OVERFLOW) {
        return Err(InitError::Hardware("initialization IRQ mailbox overflowed"));
    }
    if let Some(error) = fault {
        return Err(error);
    }
    Ok(())
}

fn rearm_consumed_sources(
    sources: &mut [RuntimeIrqSource],
    consumed: IdList,
    masked: &mut [Option<MaskedSource>; 64],
) -> Result<(), InitError> {
    for source_id in consumed.iter() {
        let Some(token) = masked.get_mut(source_id).and_then(Option::take) else {
            continue;
        };
        let source = sources
            .iter_mut()
            .find(|source| source.source_id() == source_id)
            .ok_or(InitError::MissingInterrupt)?;
        source
            .rearm(token)
            .map_err(|_| InitError::Hardware("initialization IRQ source rearm failed"))?;
    }
    Ok(())
}

pub(super) fn close_failed_registration(
    device: RdifBlockDevice,
    registrar: MaintenanceRegistrar<BlockMaintenanceEvent>,
    sources: Vec<RuntimeIrqSource>,
    error: BlockControllerError,
) -> Result<ControllerInitialization, MaintenanceError> {
    let session = registrar.activate()?;
    close_failed_session(device, session, sources, error)
}

pub(super) fn close_failed_session(
    device: RdifBlockDevice,
    session: MaintenanceSession<BlockMaintenanceEvent>,
    sources: Vec<RuntimeIrqSource>,
    error: BlockControllerError,
) -> Result<ControllerInitialization, MaintenanceError> {
    let closed = close_owner_resources(&device, session, sources)?;
    Ok(ControllerInitialization::Failed { error, closed })
}

pub(super) fn close_owner_resources(
    device: &RdifBlockDevice,
    session: MaintenanceSession<BlockMaintenanceEvent>,
    sources: Vec<RuntimeIrqSource>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    if let Err(error) = session.begin_close() {
        error!("unpublished block owner could not cut off publication: {error}");
        session.quarantine_and_park();
    }
    if let Err(error) = with_owner_irq_excluded(|| device.disable_irq()) {
        error!("unpublished block owner could not mask device sources: {error}");
        session.quarantine_and_park();
    }
    if let Err(error) = quiesce_after_device_masked(&sources) {
        error!("unpublished block owner could not drain IRQ source: {error:?}");
        session.quarantine_and_park();
    }
    if let Err(failure) = close_irq_sources(sources) {
        error!(
            "unpublished block IRQ action could not close: {:?}",
            failure.reason()
        );
        quarantine_source_close_failure(session, failure);
    }
    finish_maintenance_close(session)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RESULT_ERROR_SIZE_BUDGET: usize = 128;

    #[test]
    fn source_registration_failure_fits_the_result_error_budget() {
        assert!(
            core::mem::size_of::<SourceRegistrationFailure>() <= RESULT_ERROR_SIZE_BUDGET,
            "source-registration failure must stay compact while retaining registered sources"
        );
    }
}
