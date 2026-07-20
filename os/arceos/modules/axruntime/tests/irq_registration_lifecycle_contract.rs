use std::{fs, path::PathBuf};

fn irq_runtime_source() -> String {
    let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("ax-runtime lives four directories below the workspace root")
        .to_path_buf();
    fs::read_to_string(workspace.join("os/arceos/modules/axruntime/src/irq.rs"))
        .expect("read ax-runtime IRQ glue")
}

#[test]
fn irq_registration_reserves_fail_closed_storage_before_publishing_a_callback() {
    let source = irq_runtime_source();

    assert!(source.contains("IRQ_REGISTRATION_QUARANTINE_CAPACITY"));
    assert!(source.contains("IrqRegistrationQuarantineReservation::reserve()?"));
    assert!(source.contains("FixedQuarantineRegistry"));
    assert!(source.contains("AtomicU8"));
    assert!(source.contains("UnsafeCell<MaybeUninit<T>>"));

    let constructor = "pub(crate) fn register_shared_disabled_on(";
    let body = source
        .split_once(constructor)
        .unwrap_or_else(|| panic!("missing constructor {constructor}"))
        .1
        .split_once("\n    }")
        .expect("constructor has a bounded body")
        .0;
    let reserve = body
        .find("IrqRegistrationQuarantineReservation::reserve()?")
        .expect("constructor reserves fail-closed capacity");
    let request = body
        .find("ax_hal::irq::request_")
        .expect("constructor publishes an IRQ request");
    assert!(
        reserve < request,
        "quarantine must be reserved before request"
    );
}

#[test]
fn registration_drop_never_runs_fallible_irq_teardown() {
    let source = irq_runtime_source();
    let drop_body = source
        .split_once("impl Drop for Registration")
        .expect("Registration has Drop")
        .1
        .split_once("#[cfg(test)]")
        .expect("Drop ends before its private unit tests")
        .0;

    assert!(drop_body.contains("QuarantinedRegistration"));
    assert!(drop_body.contains("reservation.retain"));
    assert!(drop_body.contains("name"));
    assert!(drop_body.contains("handle"));
    assert!(!drop_body.contains("free_irq"));
    assert!(!drop_body.contains("disable"));
    assert!(!drop_body.contains("synchronize"));
    assert!(!drop_body.contains("self.handle.take() else"));
    assert!(!drop_body.contains("mem::forget"));
    assert!(!drop_body.contains("Box::leak"));
}

#[test]
fn detached_action_carries_the_same_quarantine_reservation_across_reattach() {
    let source = irq_runtime_source();
    let detached = source
        .split_once("pub(crate) struct DetachedRegistration")
        .expect("detached registration type exists")
        .1
        .split_once("}")
        .expect("detached registration fields")
        .0;
    assert!(detached.contains("quarantine"));
    assert!(detached.contains("released_line"));

    let detach = source
        .split_once("pub(crate) fn detach(mut self)")
        .expect("detach exists")
        .1
        .split_once("fn required_handle")
        .expect("detach body")
        .0;
    assert!(detach.contains("quarantine: self.quarantine.take()"));
    assert!(detach.contains("released_line"));

    let reattach = source
        .split_once("pub(crate) fn reattach(mut self)")
        .expect("reattach exists")
        .1
        .split_once("impl ReattachRegistrationError")
        .expect("reattach body")
        .0;
    assert!(reattach.contains("quarantine: self.quarantine.take()"));
    assert!(reattach.contains("self.released_line = released_line"));
}

#[test]
fn guest_handoff_detach_releases_the_sole_backing_line() {
    let source = irq_runtime_source();
    let detach = source
        .split_once("pub(crate) fn detach(mut self)")
        .expect("guest-handoff detach exists")
        .1
        .split_once("fn required_handle")
        .expect("guest-handoff detach body")
        .0;

    assert!(
        detach.contains("detach_irq_action_and_release_line(handle)"),
        "detaching only the callback leaves the platform IRQ-line lease owned by the host"
    );
    assert!(
        detach.contains("#[cfg(target_arch = \"riscv64\")]"),
        "the PLIC release path must be selected for the RISC-V guest handoff"
    );

    let reattach = source
        .split_once("pub(crate) fn reattach(mut self)")
        .expect("guest-return reattach exists")
        .1
        .split_once("impl ReattachRegistrationError")
        .expect("guest-return reattach body")
        .0;
    assert!(
        reattach.contains("reattach_irq_action"),
        "guest return must rebuild a fresh prepared line before publishing the callback"
    );
}

#[test]
fn consuming_close_is_available_to_every_irq_owner_and_returns_the_live_owner() {
    let source = irq_runtime_source();
    let close = source
        .split_once("pub(crate) fn close(mut self) -> Result<(), RegistrationCloseFailure>")
        .expect("generic consuming close is public")
        .1
        .split_once("/// Removes this disabled")
        .expect("close body")
        .0;

    assert!(close.contains("self.disable()"));
    assert!(close.contains("self.synchronize()"));
    assert!(close.contains("ax_hal::irq::free_irq(handle)"));
    assert!(close.contains("registration: self"));
    assert!(source.contains("pub(crate) fn into_parts(self) -> (IrqError, Registration)"));
}

#[test]
fn raw_registration_is_private_to_the_typed_maintenance_action() {
    let source = irq_runtime_source();

    assert!(source.contains("pub(crate) struct Registration"));
    assert!(!source.contains("pub struct Registration"));
    assert!(!source.contains("pub fn register_shared("));
    assert!(!source.contains("pub fn register_shared_disabled("));
    assert!(source.contains("pub(crate) fn register_shared_disabled_on("));
}
