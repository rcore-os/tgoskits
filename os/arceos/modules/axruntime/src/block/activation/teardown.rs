//! Explicit close and quarantine transactions for a block maintenance owner.

use alloc::{sync::Arc, vec::Vec};

use ax_driver::block::RdifBlockDevice;

use super::super::controller::{
    BlockController,
    source::{BlockMaintenanceEvent, RuntimeIrqSource, quiesce_after_device_masked},
};
use crate::maintenance::{MaintenanceClosed, MaintenanceError, MaintenanceSession};

pub(super) fn quarantine_controller_owner(
    controller: Arc<BlockController>,
    session: MaintenanceSession<BlockMaintenanceEvent>,
    sources: Vec<RuntimeIrqSource>,
    error: MaintenanceError,
) -> ! {
    error!(
        "block controller {} owner failed and will remain CPU-pinned in quarantine: {error}",
        controller.name()
    );
    controller.mark_offline();
    controller.quarantine_queue_endpoints(rdif_block::BlkError::Quarantined);
    let retained_sources = sources;
    match controller.disable_device_irq_on_owner() {
        Ok(()) => {
            if let Err(quiesce_error) = quiesce_after_device_masked(&retained_sources) {
                error!(
                    "block controller {} could not quiesce IRQ actions before owner quarantine: \
                     {quiesce_error:?}",
                    controller.name()
                );
            }
        }
        Err(mask_error) => {
            error!(
                "block controller {} could not mask device IRQs before owner quarantine: \
                 {mask_error}",
                controller.name()
            );
            // Without a device-side mask proof the line quench must remain in
            // force. Disable and drain only the owner action; never reopen a
            // shared backing line around an uncontained source.
            for source in &retained_sources {
                let _ = source.disable();
            }
            for source in &retained_sources {
                let _ = source.synchronize();
            }
        }
    }
    // `retained_sources` remains in this non-returning stack frame. Any action
    // that could not be disabled is still paired with the pinned owner lease;
    // late dispatch observes the closed lifecycle and contains its source.
    session.quarantine_and_park()
}

pub(super) fn close_controller_resources(
    controller: &BlockController,
    session: MaintenanceSession<BlockMaintenanceEvent>,
    sources: Vec<RuntimeIrqSource>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    if let Err(error) = session.begin_close() {
        error!("block controller close could not cut off publication: {error}");
        session.quarantine_and_park();
    }
    controller.mark_offline();
    if let Err(error) = controller.disable_device_irq_on_owner() {
        error!(
            "block controller {} could not mask device IRQs during close: {error}",
            controller.name()
        );
        controller.quarantine_queue_endpoints(rdif_block::BlkError::Quarantined);
        session.quarantine_and_park();
    }
    if let Err(error) = quiesce_after_device_masked(&sources) {
        error!(
            "block controller {} could not drain IRQ source during close: {error:?}",
            controller.name()
        );
        session.quarantine_and_park();
    }
    if let Err(failure) = close_irq_sources(sources) {
        error!(
            "block controller {} could not close an IRQ action: {:?}",
            controller.name(),
            failure.reason()
        );
        quarantine_source_close_failure(session, failure);
    }
    controller.clear_owner_link_after_drain();
    finish_maintenance_close(session)
}

pub(super) struct CloseIrqSourcesFailure {
    reason: MaintenanceError,
    _retained: Vec<RuntimeIrqSource>,
}

impl CloseIrqSourcesFailure {
    pub(super) const fn reason(&self) -> MaintenanceError {
        self.reason
    }
}

pub(super) fn close_irq_sources(
    sources: Vec<RuntimeIrqSource>,
) -> Result<(), CloseIrqSourcesFailure> {
    let mut first_error = None;
    let mut retained = Vec::new();
    for source in sources {
        if let Err(failure) = source.close() {
            let (reason, source) = failure.into_parts();
            first_error.get_or_insert(reason);
            retained.push(source);
        }
    }
    match first_error {
        None => Ok(()),
        Some(reason) => Err(CloseIrqSourcesFailure {
            reason,
            _retained: retained,
        }),
    }
}

pub(super) fn quarantine_source_close_failure(
    session: MaintenanceSession<BlockMaintenanceEvent>,
    _failure: CloseIrqSourcesFailure,
) -> ! {
    session.quarantine_and_park()
}

pub(super) fn quarantine_unpublished_owner_after_close_failure(
    _device: RdifBlockDevice,
    session: MaintenanceSession<BlockMaintenanceEvent>,
    _sources: Vec<RuntimeIrqSource>,
    _failure: CloseIrqSourcesFailure,
) -> ! {
    error!("unpublished block owner retained an IRQ action after close failure");
    session.quarantine_and_park()
}

pub(super) fn finish_maintenance_close(
    session: MaintenanceSession<BlockMaintenanceEvent>,
) -> Result<MaintenanceClosed, MaintenanceError> {
    if let Err(error) = session.try_begin_draining() {
        error!("block maintenance domain could not begin final drain: {error}");
        session.quarantine_and_park();
    }
    loop {
        match session.drain_owner(crate::maintenance::MAINTENANCE_BATCH_LIMIT, |_| {}) {
            Ok(drain) if drain.pending() => {}
            Ok(_) => break,
            Err(error) => {
                error!("block maintenance domain could not drain accepted events: {error}");
                session.quarantine_and_park();
            }
        }
    }
    if let Err(error) = session.finish_close() {
        error!("block maintenance domain could not commit close: {error}");
        session.quarantine_and_park();
    }
    match session.try_into_closed() {
        Ok(closed) => Ok(closed),
        Err(failure) => {
            let error = failure.error();
            error!("block maintenance domain lost its close proof: {error}");
            failure.into_session().quarantine_and_park();
        }
    }
}
