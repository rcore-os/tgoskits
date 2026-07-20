extern crate alloc;

// Compile the pure lifecycle model without pulling platform boot symbols into
// this host test. The module's private deterministic race tests exercise the
// exact production implementation.
#[path = "../src/maintenance/lifecycle.rs"]
mod lifecycle;

#[test]
fn included_model_covers_terminal_surface_without_platform_linkage() {
    use alloc::sync::Arc;
    use core::marker::PhantomData;

    use lifecycle::{MaintenanceClosed, MaintenanceLifecycle, MaintenanceState};

    let aborted = MaintenanceLifecycle::new();
    aborted.abort_registration();
    assert!(aborted.permits_control_access());
    assert!(aborted.permits_irq_access());
    assert!(aborted.permits_service_access());

    let quarantined = MaintenanceLifecycle::new();
    quarantined.quarantine();
    assert!(!quarantined.permits_control_access());
    assert!(!quarantined.permits_irq_access());
    assert!(!quarantined.permits_service_access());

    let lifecycle = Arc::new(MaintenanceLifecycle::new());
    let closed = MaintenanceClosed {
        lifecycle,
        _not_send: PhantomData,
    };
    assert_eq!(Arc::strong_count(&closed.lifecycle), 1);
    assert_eq!(closed.state(), MaintenanceState::Closed);
}
