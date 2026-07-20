//! Source contract for exact host-storage PCI ownership in Axvisor.

#[test]
fn x86_storage_prepare_uses_only_pci_endpoints_selected_by_the_handoff() {
    let config = include_str!("../src/config.rs");
    let manager = include_str!("../src/manager.rs");

    assert!(
        !config.contains("HOST_FILESYSTEM_RELEASE_REQUIRED")
            && !config.contains("vm_config_needs_host_filesystem_release")
            && !config.contains("host_filesystem_release_required"),
        "VM image location and generic passthrough configuration must not guess block ownership"
    );

    let prepare = config
        .split_once("fn prepare_x86_host_storage_passthrough")
        .expect("Axvisor must prepare x86 storage from the typed handoff token")
        .1
        .split_once("fn unmask_x86_qemu_block_intx")
        .expect("x86 preparation must remain separate from the activation callback")
        .0;
    assert!(prepare.contains("handoff.pci_endpoints()"));
    assert!(prepare.contains("select_x86_qemu_block_endpoint"));
    assert!(prepare.contains("Unsupported x86 host storage PCI endpoint"));

    let begin = manager
        .split_once("fn release_host_storage_for_guest_passthrough")
        .expect("Axvisor must own the host-storage transaction")
        .1
        .split_once("fn rollback_failed_guest_storage_activation")
        .expect("the transaction must retain one rollback path")
        .0;
    let selection = begin
        .find("let Some(mut handoff)")
        .expect("unrelated passthrough must not detach host storage");
    let commit = begin
        .find("commit_host_storage_handoff_to_guest")
        .expect("controller ownership must commit before PCI IRQ masking");
    let x86_prepare = begin
        .find("prepare_x86_host_storage_passthrough(&handoff)")
        .expect("x86 preparation must consume the exact handoff token");
    assert!(selection < commit && commit < x86_prepare);

    let rollback = manager
        .split_once("fn rollback_failed_guest_storage_activation")
        .expect("post-commit preparation failure needs a guest-owned rollback")
        .1
        .split_once("#[cfg(not(feature = \"fs\"))]")
        .expect("the fs transaction must remain separate")
        .0;
    let revoke = rollback
        .find("revoke_guest_storage_routes(&handoff, routes_revoked)")
        .expect("post-commit rollback must first revoke the exact guests");
    let return_storage = rollback
        .find("return_host_storage_from_guest(&mut handoff, revoked)")
        .expect("post-commit rollback must reinitialize controllers and remount storage");
    assert!(revoke < return_storage);
    assert!(!rollback.contains("abort_host_storage_handoff_before_guest"));
}

#[test]
fn x86_irq_route_is_not_registered_while_parsing_vm_configuration() {
    let config = include_str!("../src/config.rs");

    let init = config
        .split_once("pub fn init_guest_vm")
        .expect("VM creation entry must exist")
        .1
        .split_once("pub(crate) fn build_axvm_config")
        .expect("VM creation must remain a focused orchestration")
        .0;
    assert!(!init.contains("register_x86_qemu_block_irq_route"));

    let prepare = config
        .split_once("fn prepare_x86_host_storage_passthrough")
        .expect("handoff-bound x86 preparation must exist")
        .1
        .split_once("fn unmask_x86_qemu_block_intx")
        .expect("preparation must remain separate from activation")
        .0;
    let validate = prepare
        .find("select_x86_qemu_block_endpoint")
        .expect("the selected endpoint must be validated first");
    let pci_prepare = prepare
        .find("prepare_intx_passthrough")
        .expect("native PCI ownership must be prepared");
    let register = prepare
        .find("register_x86_qemu_block_irq_route")
        .expect("the guest IRQ route must be registered in the same transaction");
    let reserve_action = prepare
        .find("validate_x86_qemu_block_irq_source")
        .expect("the post-commit transaction must validate the guest IRQ source");
    assert!(
        validate < pci_prepare && pci_prepare < register && register < reserve_action,
        "host ownership must be detached and masked before the guest action claims the IRQ"
    );
}

#[test]
fn x86_irq_activation_failure_stays_in_the_vm_run_transaction() {
    let config = include_str!("../src/config.rs");
    let adapter = include_str!("../../../virtualization/axvm/src/arch/x86_64/mod.rs");
    let host_irq = include_str!("../../../virtualization/axvm/src/arch/x86_64/host_irq.rs");
    let activation = include_str!("../../../virtualization/axvm/src/arch/x86_64/irq/activation.rs");
    let state = include_str!("../../../virtualization/axvm/src/arch/x86_64/irq/state.rs");

    assert!(
        state.contains("pub struct IoApicForwardingActivationOps")
            && state.contains("activate: fn() -> AxVmResult")
            && state.contains("revoke: fn() -> AxVmResult"),
        "the endpoint capability must pair fallible activation with fallible revocation"
    );
    assert!(
        activation.contains("request required x86 IOAPIC forwarding IRQ action")
            && activation.contains("fn register_ioapic_forwarding_actions")
            && activation.contains("Err(error) =>"),
        "a required host IRQ action failure must fail first-run preparation"
    );
    let exclusive_request = host_irq
        .split_once("fn request_exclusive_irq_disabled")
        .expect("the host adapter must expose a disabled exclusive request")
        .1
        .split_once("fn synchronize_irq")
        .expect("IRQ registration and drain must remain separate")
        .0;
    assert!(
        activation.contains("request_exclusive_irq_disabled")
            && exclusive_request.contains("AutoEnable::No")
            && !exclusive_request.contains("ShareMode::Shared"),
        "the fixed vCPU owner must install one exclusive disabled action before route activation"
    );
    assert!(
        activation.contains("restore_ioapic_forwarding_enable_publication"),
        "first-run activation failure must restore owner/enabled state immediately"
    );

    let first_run = adapter
        .split_once("fn before_first_run")
        .expect("x86 must own a first-run IRQ preparation hook")
        .1
        .split_once("fn before_vcpu_run")
        .expect("the first-run hook must remain focused")
        .0;
    assert!(
        first_run.contains("irq::enable_ioapic_irq_forwarding(vm, vcpu)?"),
        "host IRQ request or initial route activation failure must stop first guest run"
    );

    let before_run = adapter
        .split_once("fn before_vcpu_run")
        .expect("x86 must drain IRQ publications before guest entry")
        .1
        .split_once("fn after_mmio_write")
        .expect("the per-run hook must remain focused")
        .0;
    assert!(
        !before_run.contains("activate_ready_ioapic_forwarding_routes"),
        "a hook returning `()` must not swallow route activation failure"
    );

    let after_mmio = adapter
        .split_once("fn after_mmio_write")
        .expect("vIOAPIC programming must have a fallible post-write hook")
        .1
        .split_once("fn handle_vcpu_exit_bound")
        .expect("the post-write hook must remain separate from exit dispatch")
        .0;
    assert!(after_mmio.contains("irq::activate_ready_ioapic_forwarding_routes(vm)"));

    let callbacks = config
        .split_once("fn unmask_x86_qemu_block_intx")
        .expect("selected PCI endpoint must provide an activation callback")
        .1
        .split_once("fn x86_qemu_block_endpoint")
        .expect("the callback must remain focused")
        .0;
    assert!(callbacks.contains("-> AxVmResult"));
    assert!(callbacks.contains("unmask_intx_passthrough(info)"));
    assert!(callbacks.contains("fn mask_x86_qemu_block_intx"));
    assert!(callbacks.contains("prepare_intx_passthrough(info)"));
    assert!(
        !callbacks.contains("warn!"),
        "PCI endpoint activation and revoke failures must be returned instead of discarded"
    );
}

#[test]
fn interactive_passthrough_start_fails_closed_without_an_owned_transaction() {
    let manager = include_str!("../src/manager.rs");

    let start = manager
        .split_once("pub fn start_vm")
        .expect("Axvisor must expose the interactive VM start operation")
        .1
        .split_once("pub fn stop_vm")
        .expect("interactive start must remain a focused operation")
        .0;
    assert!(
        start.contains("ensure_interactive_operation_has_no_passthrough"),
        "an interactive VM must not bypass the host-storage ownership coordinator"
    );

    let reset = manager
        .split_once("pub fn reset_vm")
        .expect("Axvisor must expose the interactive VM reset operation")
        .1
        .split_once("pub fn remove_vm")
        .expect("interactive reset must remain separate from removal")
        .0;
    assert!(
        reset.contains("ensure_interactive_operation_has_no_passthrough"),
        "reset restarts a VM and must obey the same passthrough ownership gate"
    );

    let remove = manager
        .split_once("pub fn remove_vm")
        .expect("Axvisor must expose interactive VM removal")
        .1
        .split_once("pub fn with_vm")
        .expect("interactive removal must remain focused")
        .0;
    assert!(
        remove.contains("ensure_interactive_operation_has_no_passthrough"),
        "removal must not discard a passthrough VM outside the retained ownership transaction"
    );
}

#[test]
fn default_vm_start_failure_is_reported_only_after_storage_cleanup() {
    let manager = include_str!("../src/manager.rs");
    let start = manager
        .split_once("pub fn start_default_vms")
        .expect("Axvisor must expose default VM orchestration")
        .1
        .split_once("pub fn create_vm_from_toml")
        .expect("default VM orchestration must remain focused")
        .0;
    let run = start
        .find("let run_report = self.runtime.start_default_vms()")
        .expect("AxVM must return typed default-start failures");
    let cleanup = start
        .find("let cleanup_result = self.finish_default_guest_storage()")
        .expect("guest routes and host storage must always be cleaned up");
    let combine = start
        .find("finish_default_vm_run(run_report, cleanup_result)")
        .expect("startup and cleanup failures must be combined after cleanup");
    assert!(run < cleanup && cleanup < combine);

    let finish = manager
        .split_once("fn finish_default_vm_run")
        .expect("Axvisor must preserve both failure domains")
        .1
        .split_once("fn format_default_vm_start_failures")
        .expect("failure formatting must remain separate from orchestration")
        .0;
    assert!(finish.contains("(Err(cleanup_error), Some(failures))"));
    assert!(finish.contains("default VM startup also failed"));
}

#[test]
fn default_guest_route_lease_is_retained_even_without_a_storage_handoff() {
    let manager = include_str!("../src/manager.rs");
    assert!(manager.contains("guest_irq_route_lease: Option<axvm::GuestIrqRouteLease>"));

    let release = manager
        .split_once("fn release_host_storage_for_guest_passthrough")
        .expect("Axvisor must own route activation")
        .1
        .split_once("fn rollback_failed_guest_storage_activation")
        .expect("activation and rollback must remain adjacent")
        .0;
    let no_controller = release
        .split_once("else {")
        .expect("no-controller selection must be explicit")
        .1
        .split_once("};")
        .expect("the no-controller branch must remain focused")
        .0;
    assert!(no_controller.contains("guest_irq_route_lease = Some(route_lease)"));
    assert!(
        manager.contains("revoke_guest_irq_route_lease(route_lease)"),
        "cleanup must revoke through the retained typed lease"
    );
}
