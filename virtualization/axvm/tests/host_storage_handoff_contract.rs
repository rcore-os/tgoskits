//! Source-level contract for host storage ownership transfer.

#[test]
fn host_storage_return_requires_controller_reinitialization_and_route_revocation() {
    let host = include_str!("../src/host/arceos.rs");
    let storage = include_str!("../src/host/storage.rs");
    let runtime = include_str!("../../../os/arceos/modules/axruntime/src/block/handoff/mod.rs");

    assert!(
        host.contains("pub fn begin_host_storage_handoff"),
        "AxVM must expose one typed host-storage handoff entry"
    );
    assert!(host.contains("prepare_runtime_controllers_for_passthrough"));
    assert!(storage.contains("PreparedBlockHandoff"));
    assert!(storage.contains("GuestOwnedBlockControllers"));
    assert!(storage.contains("GuestStorageRoutesRevoked"));
    assert!(runtime.contains("GuestAccessRevoked"));
    assert!(!runtime.contains("GuestOwnershipRevoked"));
    let return_path = host
        .split_once("pub fn return_host_storage_from_guest")
        .expect("AxVM must expose a guest-return path")
        .1
        .split_once("impl HostPlatform")
        .expect("host-storage return must remain separate from platform setup")
        .0;
    let controller_return = return_path
        .find("handoff.return_controllers(revoked)")
        .expect("guest exit must first run proof-gated controller return");
    let remount = return_path
        .find("begin_filesystem_remount")
        .expect("filesystem remount must follow controller recovery");
    assert!(
        controller_return < remount,
        "the filesystem must not remount while any hardware controller remains guest-owned"
    );
}

#[test]
fn axvisor_handoff_policy_is_not_architecture_gated() {
    let manager = include_str!("../../../os/axvisor/src/manager.rs");
    let config = include_str!("../../../os/axvisor/src/config.rs");

    let release_signature = manager
        .find("fn release_host_storage_for_guest_passthrough")
        .expect("Axvisor must own one host-storage release policy");
    let release_cfg = &manager[release_signature.saturating_sub(160)..release_signature];
    assert!(
        release_cfg.contains("#[cfg(feature = \"fs\")]")
            && !release_cfg.contains("target_arch = \"x86_64\"")
            && !release_cfg.contains("target_arch = \"loongarch64\""),
        "host storage ownership transfer must use the same transaction on all architectures"
    );
    assert!(
        !config.contains("HOST_FILESYSTEM_RELEASE_REQUIRED"),
        "storage ownership must be selected from final controller resources, not a global guess"
    );
    assert!(
        config.contains("handoff.pci_endpoints()")
            && config.contains("select_x86_qemu_block_endpoint"),
        "architecture glue must receive only the PCI endpoints retained by the handoff token"
    );
}

#[test]
fn x86_guest_storage_irq_action_is_an_exclusive_disabled_owner() {
    let activation = include_str!("../src/arch/x86_64/irq/activation.rs");
    let host_irq = include_str!("../src/arch/x86_64/host_irq.rs");

    let reserve = activation
        .split_once("pub fn validate_ioapic_irq_forwarding_source")
        .expect("x86 storage passthrough must validate its host IRQ source")
        .1
        .split_once("pub fn enable_ioapic_irq_forwarding")
        .expect("x86 IRQ source validation must remain separate from owner activation")
        .0;
    assert!(
        !reserve.contains("request_exclusive_irq_disabled") && !reserve.contains("free_irq"),
        "the manager must not install a temporary action that another thread later releases"
    );

    let owner_registration = activation
        .split_once("fn register_ioapic_forwarding_actions")
        .expect("the fixed vCPU owner must install the guest action")
        .1;
    assert!(
        owner_registration.contains("request_exclusive_irq_disabled")
            && owner_registration.contains("IrqAffinity::Fixed"),
        "the owner thread must fail first-run if exclusive fixed action registration fails"
    );

    let request = host_irq
        .split_once("pub(crate) fn request_exclusive_irq_disabled")
        .expect("the host adapter must expose a typed exclusive disabled request")
        .1
        .split_once("pub(crate) fn synchronize_irq")
        .expect("IRQ registration and drain operations must remain separate")
        .0;
    assert!(request.contains("auto_enable(host_irq::AutoEnable::No)"));
    assert!(
        !request.contains("ShareMode::Shared"),
        "a shared action cannot prove exclusive device ownership"
    );
}

#[test]
fn default_guest_exit_always_runs_the_host_storage_return_stage() {
    let manager = include_str!("../../../os/axvisor/src/manager.rs");
    let runtime = include_str!("../src/runtime/mod.rs");
    let start = manager
        .split_once("pub fn start_default_vms")
        .expect("Axvisor must provide the default VM start orchestration")
        .1
        .split_once("pub fn create_vm_from_toml")
        .expect("default VM orchestration must remain focused")
        .0;

    let guest_run = start
        .find("self.runtime.start_default_vms()")
        .expect("default guests must run");
    let storage_return = start
        .find("return_host_storage_after_guest_exit")
        .expect("guest exit must attempt the host storage return transaction");
    assert!(
        guest_run < storage_return,
        "controller return and filesystem remount must happen only after guests stop"
    );
    let runtime_start = runtime
        .split_once("pub fn start()")
        .expect("AxVM must provide its blocking default-guest run loop")
        .1
        .split_once("pub(crate) fn sub_running_vm_count")
        .expect("the default-guest run loop must remain focused")
        .0;
    assert!(
        runtime_start.contains("wait_queue_wait_until(&VMM")
            && runtime_start.contains("vm_count == 0"),
        "start_default_vms must return only after every counted guest has stopped"
    );
}

#[test]
fn default_guest_runtime_is_counted_before_its_vcpu_task_can_exit() {
    let runtime = include_str!("../src/runtime/mod.rs");
    let start = runtime
        .split_once("pub fn start()")
        .expect("AxVM must provide the blocking default-guest run loop")
        .1
        .split_once("pub(crate) fn sub_running_vm_count")
        .expect("running-count ownership must remain part of runtime orchestration")
        .0;
    let reserve = start
        .find("RunningVmStartPermit::reserve()")
        .expect("the VMM must reserve the running count before starting a VM");
    let spawn = start
        .find("match vm.start()")
        .expect("the VMM must start each registered VM");
    assert!(
        reserve < spawn,
        "a newly spawned vCPU may exit immediately, so count publication must happen first"
    );
    assert!(
        start.contains("vm.take_startup_failure()"),
        "first-vCPU preparation failures must survive until resource cleanup can report them"
    );

    let vcpus = include_str!("../src/runtime/vcpus.rs");
    let first_run_failure = vcpus
        .split_once("if let Err(error) = CurrentArch::before_first_run")
        .expect("vCPU startup must preserve its fallible architecture hook")
        .1
        .split_once("if !runtime.try_mark_vcpu_running()")
        .expect("startup failure handling must finish before Running publication")
        .0;
    assert!(first_run_failure.contains("fail_vcpu_startup("));

    let fail_vcpu_startup = vcpus
        .split_once("fn fail_vcpu_startup(")
        .expect("first-run failures must use one common state transition")
        .1
        .split_once("fn close_failed_start_irq_owner(")
        .expect("startup failure publication must precede owner cleanup")
        .0;
    let record = fail_vcpu_startup
        .find("vm.record_startup_failure(error.clone())")
        .expect("the startup error must survive until resource cleanup reports it");
    let stop = fail_vcpu_startup
        .find("vm.stop(StopReason::Fault")
        .expect("a failed architecture hook must stop the VM");
    let release_count = fail_vcpu_startup
        .find("runtime.mark_vcpu_startup_failed()")
        .expect("startup failure must release the pre-published running count");
    assert!(record < stop && stop < release_count);
}

#[test]
fn post_commit_route_failure_returns_exact_storage_or_retains_the_token() {
    let manager = include_str!("../../../os/axvisor/src/manager.rs");
    let rollback = manager
        .split_once("fn rollback_failed_guest_storage_activation")
        .expect("Axvisor must own one post-commit activation rollback path")
        .1
        .split_once("#[cfg(not(feature = \"fs\"))]")
        .expect("fs and no-fs storage orchestration must remain separate")
        .0;

    let revoke = rollback
        .find("revoke_guest_storage_routes(&handoff, routes_revoked)")
        .expect("post-commit failure must revoke the exact retained guest routes");
    let return_storage = rollback
        .find("return_host_storage_from_guest(&mut handoff, revoked)")
        .expect("post-commit failure must reinitialize controllers and remount the filesystem");
    let first_retain = rollback
        .find("self.host_storage_handoff = Some(handoff)")
        .expect("failed route revocation must retain the fail-closed token");
    let last_retain = rollback
        .rfind("self.host_storage_handoff = Some(handoff)")
        .expect("failed controller return must retain the fail-closed token");
    assert!(revoke < return_storage);
    assert!(
        rollback
            .matches("self.host_storage_handoff = Some(handoff)")
            .count()
            >= 2
    );
    assert!(first_retain < return_storage);
    assert!(return_storage < last_retain);
    assert!(rollback.contains("fail-closed") || rollback.contains("failed closed"));
}

#[test]
fn prepared_abort_cancels_reservations_without_fabricating_guest_ownership() {
    let host = include_str!("../src/host/arceos.rs");
    let abort = host
        .split_once("pub fn abort_host_storage_handoff_before_guest")
        .expect("AxVM must expose pre-guest rollback")
        .1
        .split_once("fn rollback_failed_filesystem_detach")
        .expect("pre-guest rollback must remain focused")
        .0;

    assert!(abort.contains("cancel_prepared"));
    assert!(abort.contains("complete_return"));
    assert!(!abort.contains("commit_to_guest"));
}

#[test]
fn filesystem_freeze_is_rescheduled_until_generation_leases_drain() {
    let host = include_str!("../src/host/arceos.rs");
    let begin = host
        .split_once("pub fn begin_host_storage_handoff")
        .expect("AxVM must expose one host-storage handoff entry")
        .1
        .split_once("pub fn commit_host_storage_handoff_to_guest")
        .expect("filesystem preparation must finish before controller commit")
        .0;

    let freeze = begin
        .find("begin_filesystem_freeze")
        .expect("handoff must close filesystem admission");
    let wait = begin
        .find("wait_for_filesystem_freeze")
        .expect("handoff must reschedule while old generation leases drain");
    let detach = begin
        .find("detach_filesystem")
        .expect("filesystem I/O may run only after the freeze drains");
    assert!(freeze < wait && wait < detach);

    let wait_path = host
        .split_once("fn wait_for_filesystem_freeze")
        .expect("AxVM must implement the task-context freeze wait")
        .1
        .split_once("pub fn commit_host_storage_handoff_to_guest")
        .expect("freeze waiting must remain part of handoff preparation")
        .0;
    assert!(wait_path.contains("poll_filesystem_freeze"));
    assert!(wait_path.contains("FsFreezeProgress::Pending"));
    assert!(wait_path.contains("thread::yield_now()"));
    assert!(
        !wait_path.contains("cancel_filesystem_freeze"),
        "a pending generation drain is progress, not a rollback-worthy failure"
    );
}

#[test]
fn guest_route_revocation_proof_is_created_only_by_the_revocation_transaction() {
    let storage = include_str!("../src/host/storage.rs");
    let manager = include_str!("../../../os/axvisor/src/manager.rs");

    assert!(
        !storage.contains("pub unsafe fn new() -> Self"),
        "a caller must not be able to fabricate the guest-route revocation proof"
    );
    assert!(
        storage.contains("pub fn revoke_guest_storage_routes"),
        "AxVM must expose one checked route-revocation transaction"
    );

    let return_stage = manager
        .split_once("fn return_host_storage_after_guest_exit")
        .expect("Axvisor must own the guest storage return stage")
        .1
        .split_once("#[cfg(not(feature = \"fs\"))]")
        .expect("fs and no-fs return paths must remain separate")
        .0;
    let revoke = return_stage
        .find("revoke_guest_storage_routes")
        .expect("stopped guests must revoke their access before controller recovery");
    let controller_return = return_stage
        .find("return_host_storage_from_guest")
        .expect("the proof-gated controller return must still run");
    assert!(
        revoke < controller_return,
        "IRQ/MMIO routes must be revoked and drained before controller recovery"
    );
}

#[test]
fn route_revocation_seals_passthrough_stage2_access_before_producing_proof() {
    let storage = include_str!("../src/host/storage.rs");
    let vm = include_str!("../src/vm/mod.rs");
    let access = include_str!("../src/vm/passthrough_access.rs");

    let transaction = storage
        .split_once("pub fn revoke_guest_storage_routes")
        .expect("AxVM must implement a checked route-revocation transaction")
        .1
        .split_once("enum ControllerOwnership")
        .expect("route revocation must remain separate from controller ownership")
        .0;
    assert!(
        transaction.contains("revoke_passthrough_access"),
        "route revocation must remove the guest stage-2 passthrough mappings"
    );
    assert!(
        access.contains("PassthroughAccessState")
            && vm.contains("ensure_passthrough_access_active"),
        "a revoked VM must not silently rebuild passthrough mappings on restart"
    );
}

#[test]
fn route_revocation_never_treats_an_unreadable_vm_as_not_passthrough() {
    let storage = include_str!("../src/host/storage.rs");
    let access = include_str!("../src/vm/passthrough_access.rs");

    assert!(
        access.contains("uses_passthrough_access(&self) -> AxVmResult<bool>"),
        "passthrough discovery must preserve VM resource-access failures"
    );
    assert!(
        !access.contains(".unwrap_or(false)"),
        "an unavailable VM resource set must fail closed instead of being skipped"
    );
    assert!(
        storage.contains(".uses_passthrough_access()")
            && storage.contains("map_err(|error| route_revocation_error"),
        "the route transaction must translate discovery failures into its typed error"
    );
    assert!(
        access.contains("passthrough_interrupt_mode(&self) -> AxVmResult<VMInterruptMode>"),
        "IRQ ownership discovery must also preserve VM resource-access failures"
    );
    assert!(
        !storage.contains("vm.interrupt_mode()"),
        "the proof transaction must not use a convenience query with a fallback mode"
    );
}

#[test]
fn storage_handoff_is_bound_to_selected_guests_and_final_hpa_ranges() {
    let host = include_str!("../src/host/arceos.rs");
    let storage = include_str!("../src/host/storage.rs");
    let access = include_str!("../src/vm/passthrough_access.rs");
    let manager = include_str!("../../../os/axvisor/src/manager.rs");

    assert!(
        access.contains("passthrough_host_ranges") && access.contains("mapping.hpa.as_usize()"),
        "controller selection must consume final HPA mappings instead of configured GPA or names"
    );
    assert!(
        storage.contains("StorageGuestSelection")
            && storage.contains("selected_guest_keys")
            && storage.contains("guests: Box<[AxVMRef]>")
            && host.contains("prepare_runtime_controllers_for_passthrough(")
            && host.contains("selection.regions()"),
        "the handoff token must pin exactly the guests selected by controller resource ownership"
    );
    assert!(
        storage.contains("pub fn revoke_guest_storage_routes(")
            && storage.contains("handoff.guests()")
            && manager.contains("revoke_guest_storage_routes(handoff, routes_revoked)"),
        "guest return must revoke only the VM objects retained by the handoff token"
    );
    assert!(
        host.contains("Result<Option<HostStorageHandoff>")
            && host.contains("if prepared.is_empty()")
            && manager.contains("let Some(mut handoff)"),
        "passthrough unrelated to block controllers must not detach the host filesystem"
    );
}

#[test]
fn loongarch_passthrough_irq_routes_activate_after_storage_commit_not_vm_construction() {
    let config = include_str!("../../../os/axvisor/src/config.rs");
    let loongarch = include_str!("../src/arch/loongarch64/mod.rs");
    let irq_routes = include_str!("../src/host/irq_routes.rs");
    let manager = include_str!("../../../os/axvisor/src/manager.rs");

    assert!(
        !config.contains("register_loongarch_passthrough_irq_routes(vm_id)"),
        "VM construction may still read host storage and must not steal its completion IRQ"
    );
    assert!(
        loongarch.contains("fn activate_guest_irq_routes")
            && loongarch.contains("irq::register_guest_irq_route")
            && irq_routes.contains("CurrentArch::activate_guest_irq_routes"),
        "LoongArch routes must activate through the common retained route lease"
    );
    assert!(
        !manager.contains("activate_loongarch_default_passthrough_irq_routes")
            && !manager.contains("rollback_loongarch_passthrough_irq_routes"),
        "Axvisor must not own an architecture-specific route lifetime beside the common lease"
    );
}

#[test]
fn aarch64_passthrough_irq_routes_activate_after_storage_commit_not_vm_construction() {
    let aarch64_vm = include_str!("../src/arch/aarch64/vm.rs");
    let architecture = include_str!("../src/architecture/ops.rs");
    let irq_routes = include_str!("../src/host/irq_routes.rs");
    let manager = include_str!("../../../os/axvisor/src/manager.rs");

    let initialization = aarch64_vm
        .split_once("fn init_vm_with")
        .expect("AArch64 must expose a focused VM construction path")
        .1
        .split_once("fn build_vcpu_setup_config")
        .expect("AArch64 VM construction must remain separate from route activation")
        .0;
    assert!(
        !initialization.contains("assign_passthrough_spis"),
        "VM construction may still use the host filesystem and must not reroute its completion IRQ"
    );
    assert!(
        architecture.contains("fn activate_guest_irq_routes")
            && irq_routes.contains("pub fn activate_guest_irq_routes")
            && irq_routes.contains("CurrentArch::activate_guest_irq_routes"),
        "post-commit AArch64 route activation must cross the common typed ownership boundary"
    );
    let route_registration = aarch64_vm
        .split_once("fn register_arch_devices")
        .expect("AArch64 must keep construction-time device registration focused")
        .1
        .split_once("pub(crate) fn activate_guest_irq_routes")
        .expect("post-selection activation must be a separate operation")
        .0;
    assert!(
        !route_registration.contains("assign_passthrough_spis"),
        "VM construction must not assign physical SPIs in either feature mode"
    );

    let release = manager
        .split_once("fn release_host_storage_for_guest_passthrough")
        .expect("Axvisor must own the host-storage release transaction")
        .1
        .split_once("fn rollback_failed_guest_storage_activation")
        .expect("post-commit route activation must retain a rollback path")
        .0;
    let commit = release
        .find("commit_host_storage_handoff_to_guest")
        .expect("controller ownership must commit first");
    let activate = commit
        + release[commit..]
            .find("activate_guest_irq_routes(&mut route_lease)")
            .expect("the committed-controller branch must activate architecture IRQ routes");
    assert!(commit < activate);
}

#[test]
fn post_selection_irq_routes_have_a_retained_lifecycle_lease() {
    let architecture = include_str!("../src/architecture/ops.rs");
    let irq_routes = include_str!("../src/host/irq_routes.rs");
    let manager = include_str!("../../../os/axvisor/src/manager.rs");

    assert!(
        irq_routes.contains("pub struct GuestIrqRouteLease")
            && irq_routes.contains("pub fn revoke_guest_irq_route_lease"),
        "post-selection IRQ routes need an explicit owner even when no block controller matched"
    );
    assert!(
        architecture.contains("fn activate_guest_irq_routes")
            && architecture.contains("AxVmResult"),
        "the architecture hook must participate in the retained route lifecycle"
    );
    assert!(
        manager.contains("guest_irq_route_lease: Option<axvm::GuestIrqRouteLease>"),
        "Axvisor must retain the route lease across the complete default-guest runtime"
    );

    let release = manager
        .split_once("fn release_host_storage_for_guest_passthrough")
        .expect("Axvisor must own post-selection route activation")
        .1
        .split_once("fn rollback_failed_guest_storage_activation")
        .expect("route activation must retain a focused rollback path")
        .0;
    assert!(
        release.contains("activate_guest_irq_routes(&mut route_lease)"),
        "the no-controller case must still publish route ownership into a retained lease"
    );

    let finish = manager
        .split_once("fn finish_default_guest_storage")
        .expect("Axvisor must own default-guest cleanup")
        .1
        .split_once("pub fn create_vm_from_toml")
        .expect("default-guest cleanup must remain focused")
        .0;
    let revoke = finish
        .find("ensure_default_guest_irq_routes_revoked")
        .expect("every retained post-selection route must be revoked after guests stop");
    let controller_return = finish
        .find("return_host_storage_after_guest_exit")
        .expect("controller return must remain part of default-guest cleanup");
    assert!(revoke < controller_return);
}

#[test]
fn irq_route_revocation_proof_is_consumed_by_storage_return_without_double_revoke() {
    let architecture = include_str!("../src/architecture/ops.rs");
    let irq_routes = include_str!("../src/host/irq_routes.rs");
    let storage = include_str!("../src/host/storage.rs");
    let manager = include_str!("../../../os/axvisor/src/manager.rs");

    assert!(
        irq_routes.contains("pub struct GuestIrqRoutesRevoked"),
        "successful route teardown must produce a typed proof"
    );
    let route_revoke = irq_routes
        .split_once("pub fn revoke_guest_irq_route_lease")
        .expect("the retained route lease must expose explicit revocation")
        .1
        .split_once("fn activation_error")
        .expect("route revocation must remain a focused operation")
        .0;
    assert!(
        route_revoke.contains("Result<GuestIrqRoutesRevoked"),
        "route revocation must return proof instead of only unit success"
    );

    let storage_revoke = storage
        .split_once("pub fn revoke_guest_storage_routes")
        .expect("storage return must revoke guest access")
        .1
        .split_once("fn route_revocation_error")
        .expect("storage revocation must remain focused")
        .0;
    assert!(
        storage_revoke.contains("routes_revoked: &GuestIrqRoutesRevoked"),
        "stage-2 storage revocation must require the prior route proof"
    );
    assert!(
        !storage_revoke.contains("CurrentArch::revoke_guest_irq_routes"),
        "storage return must not revoke an architecture route a second time"
    );

    let activation = irq_routes
        .split_once("pub fn activate_guest_irq_routes")
        .expect("route lifecycle activation must be explicit")
        .1
        .split_once("pub fn revoke_guest_irq_route_lease")
        .expect("activation and revocation must remain adjacent")
        .0;
    assert!(
        activation.contains("route_lease.inner_mut().guests.push(vm.clone())"),
        "the lease must retain every passthrough guest even when its architecture activates later"
    );
    assert!(
        !architecture.contains("AxVmResult<bool>"),
        "a boolean cannot prove ownership of architecture route lifetime"
    );

    assert!(
        manager.contains("guest_irq_routes_revoked: Option<axvm::GuestIrqRoutesRevoked>")
            && manager.contains("revoke_guest_storage_routes(handoff, routes_revoked)"),
        "Axvisor must retain one route proof through the controller-return transaction"
    );
}
