use std::{fs, path::PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn source(path: &str) -> String {
    fs::read_to_string(crate_root().join(path)).unwrap()
}

#[test]
fn maintenance_is_not_a_device_event_workqueue_adapter() {
    let maintenance = [
        "src/maintenance/mod.rs",
        "src/maintenance/lifecycle.rs",
        "src/maintenance/mailbox.rs",
        "src/maintenance/owner_cell.rs",
        "src/maintenance/runtime.rs",
    ]
    .into_iter()
    .map(source)
    .collect::<String>();

    assert!(!maintenance.contains("queue_work_on"));
    assert!(!maintenance.contains("WorkItem"));
    assert!(!maintenance.contains("WorkQueue"));
    assert!(!maintenance.contains("todo!"));
    assert!(!maintenance.contains("unimplemented!"));
}

#[test]
fn owner_cpu_lease_and_close_proof_cover_the_complete_session() {
    let runtime = source("src/maintenance/runtime.rs");
    let lifecycle = source("src/maintenance/lifecycle.rs");

    assert!(runtime.contains("let cpu_lease = pin_current_cpu()?"));
    assert!(runtime.contains("fn quarantine_owner_forever"));
    let runner = runtime
        .split_once("pub fn run_maintenance_current")
        .expect("the maintenance runner must own the CPU lease")
        .1
        .split_once("/// Creates one fair")
        .expect("the current-thread runner must precede the spawn wrapper")
        .0;
    assert!(runner.contains("quarantine_owner_forever"));
    assert!(runner.contains("cpu_lease"));
    assert!(
        !runtime
            .split_once("pub struct MaintenanceRegistrar")
            .expect("maintenance registrar exists")
            .1
            .split_once("impl<T: Copy + Send + 'static> MaintenanceRegistrar")
            .expect("registrar fields precede its implementation")
            .0
            .contains("CurrentCpuLease"),
        "the runner, not a fallibly dropped registrar, must own the CPU lease"
    );
    assert!(
        !runtime
            .split_once("pub struct MaintenanceSession")
            .expect("maintenance session exists")
            .1
            .split_once("impl<T: Copy + Send + 'static> MaintenanceSession")
            .expect("session fields precede its implementation")
            .0
            .contains("CurrentCpuLease"),
        "the runner must keep the CPU lease across closure errors"
    );
    assert!(runtime.contains("Result<MaintenanceClosed, MaintenanceError>"));
    assert!(runtime.contains("MaintenanceCloseFailure"));
    assert!(runtime.contains("core.lifecycle.quarantine()"));
    assert!(runtime.contains("pub fn quarantine_and_park(self) -> !"));
    let quarantine = runtime
        .split_once("pub fn quarantine_and_park(self) -> !")
        .expect("a failed close must retain the pinned owner session")
        .1
        .split_once("/// Converts a terminal session")
        .expect("owner quarantine must precede successful close conversion")
        .0;
    assert!(quarantine.contains("self.core().lifecycle.quarantine()"));
    assert!(quarantine.contains("try_wait_until(|| false)"));
    assert!(lifecycle.contains("Quarantined"));
    assert!(!runtime.contains("WrongSpawnCpu"));
}

#[test]
fn hard_irq_and_owner_cell_paths_validate_local_ownership() {
    let runtime = source("src/maintenance/runtime.rs");
    let owner_cell = source("src/maintenance/owner_cell.rs");

    assert!(runtime.contains("if !ax_hal::irq::in_irq_context()"));
    assert!(runtime.contains("actual_cpu != self.core.owner_cpu"));
    assert!(runtime.contains("self.wake.thread_id() != self.core.owner_thread"));
    assert!(runtime.contains("wake_target != Some(self.core.owner_cpu)"));
    assert!(runtime.contains("OwnerPlacementMismatch"));
    assert!(runtime.contains("begin_publish()"));
    assert!(runtime.contains("self.wake.wake()"));
    assert!(owner_cell.contains("ax_kspin::IrqGuard::new()"));
    assert!(owner_cell.contains("AtomicBool"));
    assert!(!owner_cell.contains("SpinLock"));
    assert!(owner_cell.contains("domain_lease.get()).take()"));
}

#[test]
fn timed_owner_wait_uses_one_absolute_generation_checked_park() {
    let runtime = source("src/maintenance/runtime.rs");
    let timed_wait = runtime
        .split_once("pub fn wait_for_pending_until")
        .expect("maintenance session must expose an absolute-deadline wait")
        .1
        .split_once("/// Consumes at most one fixed batch")
        .expect("timed wait must precede owner drain")
        .0;

    assert!(timed_wait.contains("if core.pending_or_not_live()"));
    assert!(timed_wait.contains(".try_wait_until_deadline("));
    assert!(timed_wait.contains("Duration::from_nanos(deadline_ns)"));
    assert!(timed_wait.matches("core.pending_or_not_live()").count() >= 2);
    assert!(timed_wait.contains("MaintenanceWaitOutcome::TimedOut"));
    assert!(!timed_wait.contains("sleep("));
    assert!(!timed_wait.contains("yield_current"));
}

#[test]
fn owner_future_and_mailbox_share_one_generation_checked_park() {
    let runtime = source("src/maintenance/runtime.rs");
    let combined_wait = runtime
        .split_once("pub fn wait_for_pending_or(")
        .expect("maintenance session must compose future and mailbox readiness")
        .1
        .split_once("/// Blocks until maintenance evidence is pending")
        .expect("combined owner wait must precede the timed wait")
        .0;

    assert!(runtime.contains("pub fn has_pending(&self) -> Result<bool, MaintenanceError>"));
    assert!(combined_wait.contains("self.validate_wait_access()?"));
    assert!(combined_wait.contains(".try_wait_until(||"));
    assert!(combined_wait.contains("core.pending_or_not_live() ||"));
    assert!(combined_wait.contains("predicate.borrow_mut())()"));
    assert!(
        !combined_wait.contains("if predicate"),
        "future readiness must not be checked before a separate mailbox park"
    );
}

#[test]
fn external_handle_is_device_scoped_status_and_request_capability() {
    let runtime = source("src/maintenance/runtime.rs");

    assert!(runtime.contains("pub struct DeviceMaintenanceHandle"));
    assert!(!runtime.contains("pub type MaintenanceHandle"));
    assert!(runtime.contains("pub fn owner_cpu(&self) -> usize"));
    assert!(runtime.contains("pub fn owner_thread(&self) -> ThreadId"));
    assert!(runtime.contains("pub fn state(&self) -> MaintenanceState"));
    assert!(runtime.contains("pub fn submit_request("));
    assert!(runtime.contains("pub fn request_shutdown(&self)"));
    assert!(runtime.contains("pub struct MaintenanceThread"));
    let spawn = runtime
        .split_once("pub fn spawn_maintenance_domain")
        .expect("maintenance spawn API must exist")
        .1
        .split_once("fn classify_task_wake")
        .expect("maintenance spawn API must precede wake classification")
        .0;
    assert!(spawn.contains("Result<MaintenanceThread, MaintenanceError>"));
    assert!(
        !spawn.contains("Result<ThreadHandle, MaintenanceError>"),
        "callers must not receive a scheduler control handle for a pinned device owner"
    );
}

#[test]
fn hard_irq_and_remote_task_publication_use_separate_ingress_paths() {
    let runtime = source("src/maintenance/runtime.rs");
    let mailbox = source("src/maintenance/mailbox.rs");
    let irq_publish = runtime
        .split_once("pub fn publish_from_irq(")
        .expect("LocalIrqWake must expose the hard-IRQ publication boundary")
        .1
        .split_once("/// Returns the registered owner CPU")
        .expect("hard-IRQ publication must remain a focused operation")
        .0;

    assert!(irq_publish.contains("publish_irq_event_serialized"));
    assert!(!irq_publish.contains("begin_publish()"));
    assert!(!irq_publish.contains("compare_exchange"));
    assert!(runtime.contains("mailbox.publish_task_event(causes, request)"));
    assert!(mailbox.contains("irq_events: LocalIrqEventRing<T>"));
    assert!(mailbox.contains("task_events: TaskEventQueue<T>"));

    let irq_ring = mailbox
        .split_once("fn try_push_serialized(&self, event: T) -> bool")
        .expect("local IRQ ingress must use its serialized-producer primitive")
        .1
        .split_once("fn pop(&self) -> Option<T>")
        .expect("serialized push must precede its owner pop")
        .0;
    assert_eq!(irq_ring.matches("compare_exchange").count(), 1);
    assert!(!irq_ring.contains("loop {"));
}

#[test]
fn live_owner_can_rebind_an_irq_endpoint_without_weakening_close() {
    let runtime = source("src/maintenance/runtime.rs");
    let lifecycle = source("src/maintenance/lifecycle.rs");
    let owner_cell = source("src/maintenance/owner_cell.rs");
    let rebind = runtime
        .split_once("pub fn local_irq_wake(&self) -> Result<LocalIrqWake<T>, MaintenanceError>")
        .expect("live maintenance sessions must support endpoint replacement")
        .1
        .split_once("/// Blocks the owner")
        .expect("endpoint replacement must precede owner waiting")
        .0;

    assert!(rebind.contains("self.validate_owner()?"));
    assert!(rebind.contains("register_live_irq_capability()?"));
    assert!(lifecycle.contains("close_waits_for_a_capability_registered_while_live"));
    assert!(lifecycle.contains("MaintenanceLifecycleError::IrqCapabilitiesLive(1)"));
    let owner_rebind = owner_cell
        .split_once("impl<E: Copy + Send + 'static> MaintenanceSession<E>")
        .expect("live session must mint replacement owner-cell IRQ access")
        .1
        .split_once("enum LocalIrqRegistration")
        .expect("owner-cell live rebind must precede registration phase selection")
        .0;
    assert!(owner_rebind.contains("Arc::ptr_eq(&control.lifecycle, self.lifecycle())"));
    assert!(owner_rebind.contains("control.enter_owner_context()?"));
    assert!(owner_rebind.contains("LocalIrqRegistration::Live"));
    assert!(owner_cell.contains("MaintenanceLifecycleError::IrqCapabilitiesLive(2)"));
}

#[test]
fn irq_action_registration_and_teardown_are_owner_typed_operations() {
    let module = source("src/maintenance/mod.rs");
    let action = source("src/maintenance/action.rs");
    let runtime = source("src/maintenance/runtime.rs");

    assert!(module.contains("mod action;"));
    assert!(module.contains("pub use action::*;"));
    assert!(action.contains("pub struct MaintenanceIrqAction"));
    assert!(action.contains("PhantomData<*mut ()>"));
    assert!(action.contains("owner_cpu: usize"));
    assert!(action.contains("owner_thread: ThreadId"));
    assert!(action.contains("lifecycle: Arc<MaintenanceLifecycle>"));
    assert!(action.contains("pub fn register_shared_disabled("));
    assert!(action.contains("Registration::register_shared_disabled_on"));
    assert!(action.contains("self.validate_owner()?"));
    assert!(action.contains("pub fn close(mut self)"));
    assert!(action.contains("MaintenanceIrqCloseFailure"));
    assert!(action.contains("registration: Box::new(self)"));
    assert!(action.contains("*self.registration"));
    assert!(action.contains("impl Drop for MaintenanceIrqAction"));
    assert!(action.contains("self.lifecycle.quarantine()"));
    assert!(action.contains("if self.registration.is_some()"));
    assert!(runtime.contains("pub(crate) fn validate_owner(&self)"));
}

#[test]
fn irq_action_itself_participates_in_close_accounting() {
    let action = source("src/maintenance/action.rs");

    assert!(
        action.contains("MaintenanceIrqActionCapability"),
        "an IRQ action must keep close accounting even when its callback does not own LocalIrqWake"
    );
    assert!(
        action.contains("MaintenanceIrqActionPhase::Registering"),
        "registrar-created actions must reserve registration-phase close accounting"
    );
    assert!(
        action.contains("MaintenanceIrqActionPhase::Live"),
        "replacement actions must reserve live-session close accounting"
    );
    assert!(
        action.contains("lifecycle_capability: self.lifecycle_capability.take()"),
        "detach and reattach must transfer one linear capability instead of changing the count"
    );
    assert!(
        action.contains("capability.release()"),
        "only an explicitly completed action close may release close accounting"
    );
}

#[test]
fn irq_action_control_is_rejected_after_every_terminal_lifecycle_state() {
    let action = source("src/maintenance/action.rs");
    let lifecycle = source("src/maintenance/lifecycle.rs");

    assert!(lifecycle.contains("pub(super) fn permits_control_access(&self) -> bool"));
    assert!(lifecycle.contains("MaintenanceState::Closed | MaintenanceState::Quarantined"));
    assert!(
        action.contains("if !lifecycle.permits_control_access()"),
        "typed IRQ actions must use the lifecycle's complete terminal-state policy"
    );
    assert!(
        !action.contains("lifecycle.state() == MaintenanceState::Quarantined"),
        "action-local terminal-state lists diverge when lifecycle states evolve"
    );
}

#[test]
fn registrar_capabilities_and_activation_revalidate_the_pinned_owner() {
    let runtime = source("src/maintenance/runtime.rs");
    let registrar = runtime
        .split_once("impl<T: Copy + Send + 'static> MaintenanceRegistrar<T>")
        .expect("maintenance registrar implementation exists")
        .1
        .split_once("impl<T: Copy + Send + 'static> Drop for MaintenanceRegistrar")
        .expect("registrar implementation precedes its drop implementation")
        .0;

    let local_wake = registrar
        .split_once("pub fn local_irq_wake")
        .expect("registrar creates local IRQ wake capabilities")
        .1
        .split_once("/// Mints a cross-CPU")
        .expect("local wake creation precedes remote handle creation")
        .0;
    assert!(local_wake.contains("self.validate_owner()?"));

    let activate = registrar
        .split_once("pub fn activate")
        .expect("registrar activates the maintenance domain")
        .1
        .split_once("/// Returns the CPU")
        .expect("activation precedes owner metadata access")
        .0;
    assert!(activate.contains("self.validate_owner()?"));
    assert!(
        runtime.contains(
            "pub fn activate(mut self) -> Result<MaintenanceSession<T>, MaintenanceError>"
        )
    );
}

#[test]
fn owner_cell_irq_capabilities_are_minted_only_by_the_verified_registrar() {
    let owner_cell = source("src/maintenance/owner_cell.rs");
    let registrar_impl = owner_cell
        .split_once("impl<E: Copy + Send + 'static> MaintenanceRegistrar<E>")
        .expect("owner cells expose registrar-bound capability creation")
        .1
        .split_once("impl<E: Copy + Send + 'static> MaintenanceSession<E>")
        .expect("registrar owner-cell methods precede live-session methods")
        .0;

    let initial_pair = registrar_impl
        .split_once("pub fn local_owner_cell")
        .expect("registrar creates the initial owner/IRQ pair")
        .1
        .split_once("/// Mints an additional")
        .expect("initial pair creation precedes additional capability creation")
        .0;
    assert!(initial_pair.contains("self.validate_owner()?"));

    let additional_irq = registrar_impl
        .split_once("pub fn local_owner_irq")
        .expect("registrar creates additional IRQ access")
        .1;
    assert!(additional_irq.contains("self.validate_owner()?"));
    assert!(owner_cell.contains("OwnerValidation(#[from] MaintenanceError)"));
}

#[test]
fn empty_registration_failure_releases_the_cpu_lease_without_weakening_quarantine() {
    let runtime = source("src/maintenance/runtime.rs");
    let runner = runtime
        .split_once("pub fn run_maintenance_current")
        .expect("maintenance runner exists")
        .1
        .split_once("/// Retains the CPU lease")
        .expect("runner precedes permanent quarantine")
        .0;
    assert!(runner.contains("try_finish_safe_abort(&core)"));
    assert!(runner.contains("if try_finish_safe_abort(&core)"));
    assert!(runner.contains("Err(error)"));
    assert!(runner.contains("quarantine_owner_forever(core, cpu_lease, error)"));

    let safe_abort = runtime
        .split_once("fn try_finish_safe_abort")
        .expect("runtime has a proof-based empty abort path")
        .1
        .split_once("/// Retains the CPU lease")
        .expect("safe abort precedes permanent quarantine")
        .0;
    assert!(safe_abort.contains("MaintenanceState::Registering"));
    assert!(safe_abort.contains("abort_registration()"));
    assert!(safe_abort.contains("try_begin_draining()"));
    assert!(safe_abort.contains("mailbox.has_pending()"));
    assert!(safe_abort.contains("finish_close(false)"));
    assert!(safe_abort.contains("MaintenanceState::Live | MaintenanceState::Quarantined"));

    let spawn = runtime
        .split_once("pub fn spawn_maintenance_domain")
        .expect("maintenance spawn wrapper exists")
        .1
        .split_once("fn classify_task_wake")
        .expect("spawn wrapper precedes wake classification")
        .0;
    assert!(!spawn.contains("panic!(\"maintenance owner initialization failed"));
}

#[test]
fn failed_owner_activation_quarantines_before_any_late_service() {
    let runtime = source("src/maintenance/runtime.rs");
    let lifecycle = source("src/maintenance/lifecycle.rs");

    let classify = runtime
        .split_once("fn classify_task_wake")
        .expect("task-context publication classifies owner activation")
        .1
        .split_once("#[cfg(test)]")
        .expect("wake classification precedes unit tests")
        .0;
    assert!(classify.contains("core.lifecycle.quarantine()"));

    let irq_publish = runtime
        .split_once("pub fn publish_from_irq")
        .expect("local IRQ wake publishes one stable event")
        .1
        .split_once("/// Returns the registered owner CPU")
        .expect("IRQ publication has a bounded body")
        .0;
    assert!(irq_publish.contains("self.core.lifecycle.quarantine()"));

    assert!(lifecycle.contains("pub(super) fn permits_service_access(&self) -> bool"));
    assert!(lifecycle.contains(
        "MaintenanceState::Live | MaintenanceState::Closing | MaintenanceState::Draining"
    ));

    let drain = runtime
        .split_once("pub fn drain_owner")
        .expect("maintenance session has a bounded owner drain")
        .1
        .split_once("/// Closes publication admission")
        .expect("owner drain precedes close")
        .0;
    assert!(drain.contains("self.validate_service_access()?"));
}

#[test]
fn owner_waits_reject_terminal_lifecycle_instead_of_spinning_as_ready() {
    let runtime = source("src/maintenance/runtime.rs");
    for method in [
        "pub fn wait_for_pending(&self)",
        "pub fn has_pending(&self)",
        "pub fn wait_for_pending_or(",
        "pub fn wait_for_pending_until(",
    ] {
        let body = runtime
            .split_once(method)
            .unwrap_or_else(|| panic!("missing maintenance wait method {method}"))
            .1
            .split_once("\n    }")
            .expect("wait method has a bounded body")
            .0;
        assert!(
            body.contains("self.validate_wait_access()?"),
            "{method} must reject Closed/Quarantined instead of reporting progress"
        );
    }
}
