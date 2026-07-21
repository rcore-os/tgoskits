//! Explicit close and fail-closed transfer for the controller owner.

use alloc::{sync::Arc, vec::Vec};

use ax_driver::block::RdifBlockPublishedOwner;

use super::{
    lifecycle::ControlLifecycle, quarantine::quarantine_control_session, service::ControlIoRuntime,
};
use crate::{
    block::activation_v13::{BoundEvidenceSource, FixedOwnershipTopology, V13MaintenanceEvent},
    maintenance::{MaintenanceClosed, MaintenanceSession},
};

const CLOSE_DRAIN_BUDGET: usize = 64;

pub(super) fn close_control_session(
    session: MaintenanceSession<V13MaintenanceEvent>,
    published: RdifBlockPublishedOwner,
    control_io: ControlIoRuntime,
    sources: Vec<BoundEvidenceSource>,
    shutdown: ControlLifecycle,
    topology: Arc<FixedOwnershipTopology>,
) -> MaintenanceClosed {
    if let Err(error) = session.begin_close() {
        quarantine_control_owner(
            session,
            "close controller publication admission",
            shutdown,
            (published, control_io, sources, error, topology),
        );
    }
    loop {
        let drain = match session.drain_owner(CLOSE_DRAIN_BUDGET, |_| {}) {
            Ok(drain) => drain,
            Err(error) => quarantine_control_owner(
                session,
                "drain controller mailbox during close",
                shutdown,
                (published, control_io, sources, error, topology),
            ),
        };
        if !drain.pending() {
            break;
        }
    }
    if let Err(error) = session.try_begin_draining() {
        quarantine_control_owner(
            session,
            "enter controller maintenance drain",
            shutdown,
            (published, control_io, sources, error, topology),
        );
    }
    if let Err(error) = session.finish_close() {
        quarantine_control_owner(
            session,
            "commit controller maintenance close",
            shutdown,
            (published, control_io, sources, error, topology),
        );
    }
    match session.try_into_closed() {
        Ok(closed) => closed,
        Err(failure) => {
            let error = failure.error();
            quarantine_control_owner(
                failure.into_session(),
                "extract controller maintenance close proof",
                shutdown,
                (published, control_io, sources, error, topology),
            )
        }
    }
}

pub(super) fn quarantine_control_owner<T>(
    session: MaintenanceSession<V13MaintenanceEvent>,
    phase: &'static str,
    shutdown: ControlLifecycle,
    retained: T,
) -> ! {
    shutdown.fail_closed();
    quarantine_control_session(session, phase, (shutdown, retained))
}
