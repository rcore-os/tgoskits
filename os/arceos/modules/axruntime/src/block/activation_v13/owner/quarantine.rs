//! Fail-closed ownership sinks for the controller maintenance owner.

use super::*;

pub(super) fn quarantine_failed_control_registration<T>(
    registrar: MaintenanceRegistrar<V13MaintenanceEvent>,
    startup: Arc<OwnerStartupCell<Result<ControlOwnerReady, ControlOwnerStartupFailure>>>,
    phase: &'static str,
    retained: T,
) -> Result<crate::maintenance::MaintenanceClosed, crate::maintenance::MaintenanceError> {
    let session = registrar.activate()?;
    quarantine_failed_control_session(session, startup, phase, retained)
}

pub(super) fn quarantine_failed_control_session<T>(
    session: MaintenanceSession<V13MaintenanceEvent>,
    startup: Arc<OwnerStartupCell<Result<ControlOwnerReady, ControlOwnerStartupFailure>>>,
    phase: &'static str,
    retained: T,
) -> ! {
    let _ = startup.publish(Err(ControlOwnerStartupFailure { phase }));
    quarantine_control_session(session, phase, retained)
}

pub(super) fn quarantine_control_session<T>(
    session: MaintenanceSession<V13MaintenanceEvent>,
    phase: &'static str,
    retained: T,
) -> ! {
    error!("rdif-block v0.13 control owner entered quarantine during {phase}");
    let _retained = retained;
    session.quarantine_and_park()
}
