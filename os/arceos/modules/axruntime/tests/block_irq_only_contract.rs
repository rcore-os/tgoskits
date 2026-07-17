use std::{fs, path::PathBuf};

use ax_runtime::block::{GuestAccessRevoked, prepare_runtime_controllers_for_passthrough};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("ax-runtime must live under os/arceos/modules")
        .to_path_buf()
}

fn read_workspace_file(path: &str) -> String {
    fs::read_to_string(workspace_root().join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

fn rust_sources_under(path: &str) -> String {
    let root = workspace_root().join(path);
    let mut pending = vec![root];
    let mut source = String::new();

    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        {
            let entry = entry.expect("source directory entry must be readable");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                source.push_str(
                    &fs::read_to_string(&path).unwrap_or_else(|error| {
                        panic!("failed to read {}: {error}", path.display())
                    }),
                );
            }
        }
    }

    source
}

#[test]
fn runtime_owns_workqueue_and_block_services() {
    let manifest = read_workspace_file("os/arceos/modules/axruntime/Cargo.toml");
    let lib = read_workspace_file("os/arceos/modules/axruntime/src/lib.rs");

    assert!(manifest.contains("workqueue = [\"multitask\"]"));
    assert!(manifest.contains("block = ["));
    assert!(lib.contains("mod workqueue;"));
    assert!(lib.contains("mod block;"));
}

#[test]
fn filesystem_has_no_driver_runtime_dependencies() {
    let manifest = read_workspace_file("os/arceos/modules/axfs-ng/Cargo.toml");

    for forbidden in ["rdif-block", "irq-framework", "dma-api"] {
        assert!(
            !manifest.contains(forbidden),
            "ax-fs-ng must consume a block service instead of depending on {forbidden}"
        );
    }
}

#[test]
fn rdif_block_exposes_no_completion_polling_contract() {
    let source = rust_sources_under("drivers/interface/rdif-block/src");

    for forbidden in [
        "poll_request",
        "poll_completions",
        "RequestPoll",
        "PollError",
        "POLLED",
    ] {
        assert!(
            !source.contains(forbidden),
            "rdif-block still exposes forbidden polling contract {forbidden}"
        );
    }
}

#[test]
fn filesystem_block_path_has_no_periodic_completion_polling() {
    let source = rust_sources_under("os/arceos/modules/axfs-ng/src");

    for forbidden in [
        "BlockCompletionMode",
        "RequestPoller",
        "poll_request",
        "poll_completions",
        "irq_driven",
    ] {
        assert!(
            !source.contains(forbidden),
            "ax-fs-ng still contains forbidden block runtime symbol {forbidden}"
        );
    }
}

#[test]
fn driver_binding_layer_has_no_synchronous_polling_wrapper() {
    let source = rust_sources_under("drivers/ax-driver/src/block");
    let build = read_workspace_file("drivers/ax-driver/build.rs");
    let bare_nvme_test = read_workspace_file("drivers/test_crates/driver-tests/tests/nvme.rs");

    for forbidden in [
        "SyncBlockOps",
        "SyncBlockDevice",
        "sync_block_dev",
        "poll_request",
        "poll_until_complete",
    ] {
        assert!(
            !source.contains(forbidden)
                && !build.contains(forbidden)
                && !bare_nvme_test.contains(forbidden),
            "ax-driver still contains the forbidden synchronous block wrapper `{forbidden}`"
        );
    }
}

#[test]
fn block_drivers_have_no_runtime_completion_mode_switch() {
    let mut source = rust_sources_under("drivers/blk");
    source.push_str(&rust_sources_under("drivers/ax-driver/src/block"));

    assert!(
        !source.contains("irq_driven"),
        "hardware block completion is an IRQ-only queue property, not a runtime boolean"
    );
}

#[test]
fn worker_pools_start_after_cpus_and_before_devices() {
    let runtime = read_workspace_file("os/arceos/modules/axruntime/src/lib.rs");
    let secondary = read_workspace_file("os/arceos/modules/axruntime/src/mp.rs");
    let start_cpus = runtime
        .find("self::mp::start_secondary_cpus(cpu_id)")
        .expect("SMP runtime must start secondary CPUs");
    let start_workers = runtime
        .find("workqueue::initialize()")
        .expect("runtime must initialize shared worker pools");
    let probe_devices = runtime
        .find("devices::probe_all_devices()")
        .expect("runtime must probe devices");

    assert!(start_cpus < start_workers);
    assert!(start_workers < probe_devices);
    assert!(
        !secondary.contains("while !super::is_init_ok()"),
        "secondary CPUs must enter scheduler idle instead of spinning on system-ready"
    );
}

#[test]
fn block_irq_actions_are_registered_disabled_before_controller_publication() {
    let irq = read_workspace_file("os/arceos/modules/axruntime/src/irq.rs");

    assert!(
        irq.contains("register_shared_disabled"),
        "block activation needs an IRQ action that stays disabled until every hctx route is live"
    );
    assert!(
        irq.contains("AutoEnable::No"),
        "disabled registration must be enforced by the IRQ framework request, not by a later \
         race-prone disable"
    );
    for operation in [
        "pub fn enable(&self)",
        "pub fn disable(&self)",
        "pub fn synchronize(&self)",
    ] {
        assert!(
            irq.contains(operation),
            "runtime IRQ registration is missing lifecycle operation {operation}"
        );
    }
}

#[test]
fn initialization_deferred_ack_is_serviced_by_the_bounded_worker() {
    let rdif = read_workspace_file("drivers/interface/rdif-block/src/init.rs");
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");

    assert!(rdif.contains("pub enum InitIrqProgress"));
    assert!(rdif.contains("fn service_deferred_irq(&mut self, source_id: usize)"));
    assert!(activation.contains("struct DeferredInitializationIrqs"));
    assert!(activation.contains("sources: AtomicU64"));
    assert!(activation.contains("deferred_irqs: DeferredInitializationIrqs"));
    assert!(activation.contains("self.deferred_irqs.restore(deferred)"));
    assert!(activation.contains(".record_deferred_irq(source_id)"));
    assert!(activation.contains("initializer.service_deferred_irq(source_id)"));
    assert!(activation.contains("InitIrqProgress::Deferred"));
    assert!(
        !activation.contains("reject_deferred_irq"),
        "a deferred destructive acknowledgement is worker continuation, not initialization failure"
    );

    let worker = activation
        .split_once("fn controller_init_work_entry")
        .expect("controller initialization worker must exist")
        .1
        .split_once("fn controller_init_timer_entry")
        .expect("controller initialization worker must remain focused")
        .0;
    let acknowledge = worker
        .find("activation.service_deferred_irqs()")
        .expect("worker must service deferred destructive acknowledgements");
    let poll = worker
        .find("activation.wake.begin_poll()")
        .expect("worker must collect acknowledged initialization sources");
    assert!(acknowledge < poll);
}

#[test]
fn block_controller_owns_interface_and_irq_leases_for_queue_lifetime() {
    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let routes =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/registry.rs");

    assert!(
        controller.contains("RdifBlockDevice"),
        "runtime controller must retain the complete driver-core object, including PCI IRQ leases"
    );
    assert!(
        routes.contains("register_shared_disabled"),
        "controller routes must be installed while OS IRQ actions are disabled"
    );
    assert!(routes.contains("Registration::register_shared_disabled"));
    let register = controller
        .find("register_irq_routes_disabled(")
        .expect("activation must construct every disabled route");
    let action_enable = controller[register..]
        .find("registration.enable()")
        .map(|offset| offset + register)
        .expect("OS action enable must follow route registration");
    let driver_enable = controller[action_enable..]
        .find("enable_irq()")
        .map(|offset| offset + action_enable)
        .expect("device IRQ unmask must follow OS action enable");
    assert!(register < action_enable && action_enable < driver_enable);
}

#[test]
fn passthrough_uses_typed_prepare_commit_and_return_permits() {
    let module = read_workspace_file("os/arceos/modules/axruntime/src/block/mod.rs");
    let handoff = rust_sources_under("os/arceos/modules/axruntime/src/block/handoff");
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/lifecycle.rs");

    assert!(module.contains("prepare_runtime_controllers_for_passthrough"));
    assert!(!module.contains("detach_runtime_controllers_for_passthrough"));
    assert!(handoff.contains("PreparedBlockHandoff"));
    assert!(handoff.contains("GuestOwnedBlockControllers"));
    assert!(handoff.contains("GuestAccessRevoked"));
    assert!(!handoff.contains("GuestOwnershipRevoked"));
    assert!(handoff.contains("QuarantinedBlockControllers"));

    let detach = hctx
        .split_once("fn detach_after_dma_quiesce")
        .expect("hctx must expose a proof-gated reversible detach transition")
        .1
        .split_once("impl HardwareQueue")
        .expect("detach implementation must remain focused")
        .0;
    assert!(detach.contains("reclaim_after_quiesce"));
    assert!(
        !detach.contains("shutdown("),
        "guest-returnable hctx state must not call an irreversible queue shutdown"
    );

    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let commit = controller
        .split_once("fn commit_handoff")
        .expect("controller must have one destructive commit boundary")
        .1
        .split_once("fn drive_handoff_dma_quiesce")
        .expect("commit must end before the lifecycle driver")
        .0;
    assert!(commit.contains("ControllerPhase::Quiescing as u8"));
    assert!(commit.contains("ControllerPhase::GuestOwned as u8"));
    assert!(
        !commit.contains(".store(ControllerPhase::GuestOwned as u8"),
        "late recovery or quarantine must not be overwritten by guest ownership"
    );
}

#[test]
fn request_cancellation_uses_hctx_work_and_dma_quiesced_recovery() {
    let hctx = rust_sources_under("os/arceos/modules/axruntime/src/block/hctx");
    let service = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/service_loop.rs");
    let request = read_workspace_file("os/arceos/modules/axruntime/src/block/request.rs");

    assert!(hctx.contains("pub fn request_cancel(&self)"));
    assert!(hctx.contains("queue_service(HctxCause::Cancel)"));
    assert!(service.contains("fn service_cancellations("));
    assert!(service.contains("RecoveryCause::Cancelled"));
    assert!(request.contains("Canceling"));
    assert!(request.contains("finish_cancel_after_return"));
}

#[test]
fn guest_return_requires_an_explicit_route_revocation_proof() {
    let _prepare = prepare_runtime_controllers_for_passthrough;
    let _proof_type = core::mem::size_of::<GuestAccessRevoked>();

    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");
    let handoff = read_workspace_file("os/arceos/modules/axruntime/src/block/handoff/mod.rs");
    assert!(handoff.contains("_revoked: GuestAccessRevoked"));
    let return_path = controller
        .split_once("fn return_from_guest")
        .expect("guest-owned controllers must have one typed return path")
        .1;
    let reattach = return_path
        .find("reattach_host_actions()")
        .expect("guest return must restore the detached host handler actions");
    let begin_recovery = return_path
        .find("begin_guest_return_recovery")
        .expect("guest return must rebuild the retained hardware queues");
    assert!(reattach < begin_recovery);
    assert!(return_path.contains("begin_guest_return_recovery"));
    assert!(return_path.contains("ControllerReady"));
}

#[test]
fn passthrough_identity_uses_the_unfiltered_runtime_registry_slot() {
    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/registry.rs");
    let selection = controller
        .split_once("fn runtime_handoff_controllers")
        .expect("runtime must expose one passthrough-controller selection point")
        .1
        .split_once("static RUNTIME_CONTROLLERS")
        .expect("selection implementation must stay before the controller registry")
        .0;
    let enumerate = selection
        .find(".enumerate()")
        .expect("controller identities must retain the runtime-registry slot");
    let eligibility = selection
        .find("has_interrupt_queues()")
        .expect("inline-only controllers must not be transferred to a guest");

    assert!(
        enumerate < eligibility,
        "filtering before enumeration renumbers controllers and breaks stable identities"
    );
}

#[test]
fn quarantine_cannot_be_overwritten_by_late_recovery_publication() {
    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");
    let enable = controller
        .split_once("RecoveryStep::EnableActions =>")
        .expect("recovery must have one host-publication step")
        .1
        .split_once("RecoveryStep::Idle | RecoveryStep::Finished")
        .expect("host publication must remain inside the bounded recovery step")
        .0;

    assert!(enable.contains("compare_exchange("));
    assert!(enable.contains("ControllerPhase::Recovering as u8"));
    assert!(enable.contains("ControllerPhase::Running as u8"));
    assert!(
        !enable.contains(".store(ControllerPhase::Running as u8"),
        "an unconditional Running store can reopen a quarantined controller"
    );

    let quarantine = controller
        .split_once("fn mark_offline")
        .expect("controller must have one fail-closed transition")
        .1
        .split_once("fn schedule_recovery")
        .expect("fail-closed transition must remain separate from recovery")
        .0;
    assert!(
        quarantine.contains(".swap(ControllerPhase::Offline as u8"),
        "quarantine and Running publication must both use ordered atomic transitions"
    );
}

#[test]
fn staged_controller_initialization_runs_only_after_irq_actions_are_live() {
    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let activate = controller
        .split_once("pub fn activate")
        .map(|(_, tail)| tail)
        .expect("controller must expose activation");
    let initialize = activate
        .find("drive_controller_initialization")
        .expect("activation must drive the portable initialization endpoint");
    let materialize = activate
        .find("materialize_logical_devices(&mut device)")
        .expect("logical device geometry and queues must follow controller readiness");
    let create = activate
        .find("create_runtime_devices(logical_devices")
        .expect("runtime device views must be created after logical device materialization");

    assert!(
        initialize < materialize && materialize < create,
        "capacity and queues must remain unpublished until initialization returns Ready"
    );
}

#[test]
fn failed_initialization_has_no_controller_publication_path() {
    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/registry.rs");
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let registry = controller
        .split_once("pub fn activate_discovered_controllers")
        .map(|(_, body)| body)
        .expect("runtime must expose discovered-controller activation");
    let success = registry
        .split_once("Ok(controller) =>")
        .map(|(_, body)| body)
        .expect("only successful activation may enter the runtime registry");
    let (success, failure) = success
        .split_once("Err(error) =>")
        .expect("failed activation must have a separate fail-closed branch");

    assert!(success.contains("RUNTIME_CONTROLLERS.lock().push"));
    assert!(!failure.contains("RUNTIME_CONTROLLERS.lock().push"));
    assert!(
        activation.contains("ACTIVATION_FAILED => Err(BlockControllerError::Initialization"),
        "portable initialization failure must remain an error, not a partially ready device"
    );
}

#[test]
fn controller_stages_initialization_before_queue_and_normal_irq_publication() {
    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let discover = activation
        .find("controller_init()")
        .expect("activation must query the portable initialization capability");
    let init_sources = activation[discover..]
        .find("initializer.irq_sources()")
        .map(|offset| offset + discover)
        .expect("activation must validate initialization IRQ sources");
    let init_register = activation[init_sources..]
        .find("Registration::register_shared_disabled_on")
        .map(|offset| offset + init_sources)
        .expect("initialization IRQ actions must be registered disabled");
    let action_enable = activation[init_register..]
        .find("registration.enable()")
        .map(|offset| offset + init_register)
        .expect("initialization actions must be live before the FSM starts");
    let device_enable = activation[action_enable..]
        .find(".enable_irq()")
        .map(|offset| offset + action_enable)
        .expect("device interrupt generation must follow action enablement");
    let drive = activation[device_enable..]
        .find("queue_work_on(activation.work())")
        .map(|offset| offset + device_enable)
        .expect("the first initialization work pass must follow IRQ binding");

    assert!(
        discover < init_sources
            && init_sources < init_register
            && init_register < action_enable
            && action_enable < device_enable
            && device_enable < drive
    );

    let activate = controller
        .split_once("pub fn activate")
        .map(|(_, body)| body)
        .expect("controller must expose one activation transaction");
    let initialize = activate
        .find("drive_controller_initialization(device)")
        .expect("controller activation must wait for portable readiness");
    let materialize = activate
        .find("materialize_logical_devices(&mut device)")
        .expect("activation must materialize every controller logical device view");
    let create = activate
        .find("create_runtime_devices(logical_devices")
        .expect("activation must construct device-scoped runtime queues");
    let validate = activate
        .find("validate_lifecycle_activation")
        .expect("activation must validate the controller lifecycle endpoint");
    let register = activate[validate..]
        .find("register_irq_routes_disabled")
        .map(|offset| offset + validate)
        .expect("normal queue IRQ routes must follow lifecycle validation");

    assert!(
        initialize < materialize
            && materialize < validate
            && validate < create
            && create < register
    );

    let registry =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/registry.rs");
    let bind = registry
        .find("queue.bind_interrupt_controller(")
        .expect("every hctx must bind the retained controller identity");
    let activate_hctx = registry
        .find("HardwareQueue::activate(queue")
        .expect("bound queues must enter hardware-queue activation");
    assert!(
        bind < activate_hctx,
        "a queue must reject foreign DMA proofs before it can be published"
    );
}

#[test]
fn recovery_deferred_irq_is_acknowledged_before_reaching_init_input() {
    let registry =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/registry.rs");
    let recovery =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs");

    assert!(
        registry.contains("outcome.is_deferred()")
            && registry.contains("record_deferred_lifecycle_irq(source_id)"),
        "a deferred top-half result must use a distinct recovery handoff"
    );
    let deferred_service = recovery
        .find("service_recovery_deferred_irqs")
        .expect("recovery worker must own deferred destructive acknowledgement");
    let recovery_input = recovery
        .find("fn recovery_input")
        .expect("acknowledged sources must later enter InitInput");
    assert!(deferred_service < recovery_input);
    assert!(recovery.contains("lifecycle.service_deferred_irq(source_id)"));
    assert!(recovery.contains("InitIrqProgress::Acknowledged"));
    assert!(recovery.contains("InitIrqProgress::Deferred"));
}

#[test]
fn deferred_irq_ack_failure_is_terminal_in_activation_and_recovery() {
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let recovery =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs");

    assert!(
        activation.contains("InitIrqProgress::Failed(error) => return Err(error)"),
        "activation must fail closed when an owned deferred source cannot be acknowledged"
    );
    assert!(
        recovery.contains("InitIrqProgress::Failed(error) => return Err(error)"),
        "recovery must quarantine the controller when deferred acknowledgement fails"
    );
}

#[test]
fn deferred_irq_contention_yields_without_losing_or_fabricating_evidence() {
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let recovery =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");
    let recovery_irq =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs");

    assert!(activation.contains("self.deferred_irqs.restore(deferred)"));
    assert!(activation.contains("Ok(true) => return WorkOutcome::Requeue"));
    assert!(recovery_irq.contains("InitIrqProgress::Unhandled => {}"));
    assert!(recovery_irq.contains("InitIrqProgress::Acknowledged => acknowledged.insert"));
    assert!(recovery_irq.contains("InitIrqProgress::Deferred => deferred.insert"));
    assert!(recovery_irq.contains("fetch_or(deferred.bits(), Ordering::Release)"));
    assert!(
        recovery
            .matches("Ok(true) => return WorkOutcome::Requeue")
            .count()
            >= 2,
        "both quiesce and reinitialize recovery polls must yield on deferred acknowledgement"
    );
}

#[test]
fn activation_irq_publication_has_a_hard_irq_safe_admission_failure_path() {
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");

    assert!(activation.contains("irq_work_failed: AtomicBool"));
    assert!(activation.contains("completion_wake: ThreadWakeHandle"));
    assert!(activation.contains("fn fail_from_irq_work_admission"));
    assert!(activation.contains("self.completion_wake.wake()"));
    assert!(
        activation.contains("ActivationIrqAction::QuenchAndWake"),
        "an unacknowledged deferred source must be quenched if its fixed worker cannot run"
    );
}

#[test]
fn initialization_teardown_masks_device_before_disabling_os_actions() {
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let close = activation
        .split_once("fn close_started_routes(")
        .expect("controller initialization must own one started-route teardown")
        .1
        .split_once("#[cfg(test)]")
        .expect("started-route teardown must remain before its tests")
        .0;
    let device_mask = close
        .find(".disable_irq()")
        .expect("device interrupt generation must be masked during teardown");
    let action_disable = close
        .find("registration.disable()")
        .expect("OS actions must be disabled after the device source is masked");

    assert!(
        device_mask < action_disable,
        "an unmasked device must retain a live acknowledgement action until masking succeeds"
    );
}

#[test]
fn controller_handoff_and_recovery_mask_device_before_draining_actions() {
    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let recovery =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");
    let recovery_irq =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs");

    let handoff = controller
        .split_once("fn commit_handoff(")
        .expect("controller must expose one guest handoff commit")
        .1
        .split_once("fn drive_handoff_dma_quiesce")
        .expect("handoff IRQ teardown must remain before DMA quiescence")
        .0;
    let handoff_mask = handoff
        .find("mask_device()")
        .expect("handoff must first mask device interrupt generation");
    let handoff_action_disable = handoff
        .find("registration.disable()")
        .expect("handoff must then disable the OS actions");
    assert!(handoff_mask < handoff_action_disable);

    let emergency = recovery_irq
        .split_once("fn mask_recovery_sources(")
        .expect("controller must expose one fail-closed IRQ mask helper")
        .1
        .split_once("fn enable_recovery_irqs")
        .expect("mask and enable operations must remain separate")
        .0;
    let emergency_mask = emergency
        .find("device.lock().disable_irq()")
        .expect("an emergency path must first mask device interrupt generation");
    let emergency_disable = emergency
        .find("registration.disable()")
        .expect("an emergency path must then disable OS actions");
    assert!(emergency_mask < emergency_disable);

    let recovery = recovery
        .split_once("RecoveryStep::DisableActions =>")
        .expect("recovery must expose one source-mask/action-drain step")
        .1
        .split_once("RecoveryStep::DrainActions =>")
        .expect("action drain must follow source masking")
        .0;
    let recovery_mask = recovery
        .find("device.lock().disable_irq()")
        .expect("recovery must mask device interrupt generation");
    let recovery_disable = recovery
        .find("registration.disable_async")
        .expect("recovery must asynchronously drain each OS action");
    assert!(recovery_mask < recovery_disable);
}

#[test]
fn handoff_drains_queue_callbacks_before_requesting_dma_quiescence() {
    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let hctx = rust_sources_under("os/arceos/modules/axruntime/src/block/hctx");
    let handoff = controller
        .split_once("fn commit_handoff(")
        .expect("controller must expose one guest handoff commit")
        .1
        .split_once("fn drive_handoff_dma_quiesce")
        .expect("handoff commit must precede the DMA lifecycle driver")
        .0;

    assert!(
        hctx.contains("struct ServiceDrainedHardwareQueue"),
        "queue callback drain must be represented by a typed ownership phase"
    );
    assert!(
        hctx.contains("Result<ServiceDrainedHardwareQueue, HardwareQueueError>"),
        "the quiesced permit must transform into the callback-drained permit"
    );
    let detach_irq = handoff
        .find("irq_owner.detach_actions()")
        .expect("host IRQ actions must be detached before queue callback drain");
    let drain_service = handoff
        .find("drain_service_work()")
        .expect("every quiesced hctx must cancel watchdog and flush service work");
    let dma_quiesce = handoff
        .find("drive_handoff_dma_quiesce()")
        .expect("controller DMA quiescence must follow callback drain");
    assert!(detach_irq < drain_service && drain_service < dma_quiesce);
}

#[test]
fn block_irq_route_is_affined_to_its_shared_worker_cpu() {
    let irq = read_workspace_file("os/arceos/modules/axruntime/src/irq.rs");
    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/registry.rs");

    assert!(irq.contains("IrqAffinity::Fixed(CpuId(cpu))"));
    assert!(controller.contains("register_shared_disabled_on"));
    assert!(controller.contains("routes[0].affinity_cpu()"));
}

#[test]
fn irq_event_bridge_failure_quenches_the_action_before_recovery_wake() {
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/irq_publication.rs");
    let routes =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/registry.rs");
    let irq_return = read_workspace_file("components/irq-framework/src/types.rs");

    assert!(hctx.contains("HardwareQueueError::EventOverflow"));
    assert!(hctx.contains("queue.control.raise(HctxCause::EventOverflow)"));
    assert!(hctx.contains("queue.queue_service_work()"));
    assert!(hctx.contains("queue.record_irq_service_error(&error)"));
    assert!(routes.contains("let mut quench = false"));
    assert!(routes.contains("quench = true"));
    assert!(routes.contains("IrqReturn::QuenchAndWake"));
    assert!(irq_return.contains("QuenchAndWake"));
}

#[test]
fn recovery_releases_a_quenched_shared_line_only_after_device_masking() {
    let recovery =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");
    let recovery_irq =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs");
    let helper = recovery_irq
        .split_once("fn mask_recovery_sources")
        .expect("recovery must own one emergency mask helper")
        .1
        .split_once("fn enable_recovery_irqs")
        .expect("emergency masking must remain separate from IRQ enablement")
        .0;
    let device_mask = helper
        .find("self.device.lock().disable_irq()")
        .expect("device-side interrupt delivery must be masked first");
    let mask_failure = helper[device_mask..]
        .find("return false;")
        .map(|offset| offset + device_mask)
        .expect("a failed device mask must retain the line quench");
    let release = helper
        .find("release_registration_quenches")
        .expect("successful device masking must release the emergency quench");
    assert!(device_mask < mask_failure && mask_failure < release);

    let disable_step = recovery
        .split_once("RecoveryStep::DisableActions =>")
        .expect("bounded recovery must have one action-disable step")
        .1
        .split_once("RecoveryStep::DrainActions =>")
        .expect("action disable must precede action drain")
        .0;
    let device_mask = disable_step
        .find("self.device.lock().disable_irq()")
        .expect("bounded recovery must mask the device source");
    let release = disable_step
        .find("release_registration_quenches")
        .expect("bounded recovery must release quench ownership");
    let action_disable = disable_step
        .find("registration.disable_async")
        .expect("bounded recovery must then drain its disabled actions");
    assert!(device_mask < release && release < action_disable);
}

#[test]
fn passthrough_handoff_has_one_runtime_owned_transaction_entry() {
    let _prepare = prepare_runtime_controllers_for_passthrough;

    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let stop_submit = controller
        .find("fn commit_handoff")
        .expect("controller handoff must close service admission");
    let own_actions = controller[stop_submit..]
        .find("HandoffIrqOwner::take(self)")
        .map(|offset| offset + stop_submit)
        .expect("handoff must retain IRQ registrations across every early return");
    let disable_device = controller[own_actions..]
        .find("mask_device()")
        .map(|offset| offset + own_actions)
        .expect("controller handoff must mask device IRQ generation first");
    let disable_action = controller[disable_device..]
        .find("registration.disable()")
        .map(|offset| offset + disable_device)
        .expect("controller handoff must close OS IRQ action admission");
    let synchronize = controller[disable_action..]
        .find("synchronize")
        .map(|offset| offset + disable_action)
        .expect("controller handoff must synchronize hard IRQ callbacks");
    let proof = controller[synchronize..]
        .find("drive_handoff_dma_quiesce")
        .map(|offset| offset + synchronize)
        .expect("controller handoff must obtain typed DMA quiescence");
    let detach = controller[proof..]
        .find("permit.detach_after_dma_quiesce(proof)")
        .map(|offset| offset + proof)
        .expect("queue detach must consume the typed DMA proof");
    let guest_owned = controller[detach..]
        .find("lifecycle.enter_guest_owned(proof)")
        .map(|offset| offset + detach)
        .expect("controller lifecycle must consume the old proof before guest execution");
    let retain = controller[guest_owned..]
        .find("irq_owner.publish_detached_actions()")
        .map(|offset| offset + guest_owned)
        .expect("detached IRQ actions must remain retained after DMA-safe queue detach");
    assert!(
        stop_submit < own_actions
            && own_actions < disable_device
            && disable_device < disable_action
            && own_actions < disable_action
            && disable_action < synchronize
            && synchronize < proof
            && proof < detach
            && detach < guest_owned
            && guest_owned < retain
    );
}

#[test]
fn passthrough_removes_host_irq_actions_before_guest_owned_publication() {
    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let commit = controller
        .split_once("fn commit_handoff")
        .expect("controller must expose one guest handoff commit")
        .1
        .split_once("fn drive_handoff_dma_quiesce")
        .expect("IRQ ownership transfer must remain in the handoff transaction")
        .0;

    let synchronized = commit
        .find("registration.synchronize()")
        .expect("host IRQ callbacks must drain before their actions are removed");
    let detached = commit
        .find("irq_owner.detach_actions()")
        .expect("host IRQ actions must be removed while preserving their handler ownership");
    let retained = commit
        .find("irq_owner.publish_detached_actions()")
        .expect("detached host callbacks must have a shutdown-lifetime owner");
    let guest_owned = commit
        .find("ControllerPhase::GuestOwned as u8")
        .expect("the controller must publish guest ownership exactly once");

    assert!(synchronized < detached && detached < retained && retained < guest_owned);
    assert!(
        !commit.contains("irq_owner.restore()"),
        "a disabled host action still occupies the descriptor and can create dual IRQ ownership"
    );
}

#[test]
fn accepted_irq_snapshot_enters_recovery_if_service_work_cannot_be_queued() {
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/irq_publication.rs");
    let record = hctx
        .split_once("pub fn record_irq_event")
        .expect("hardware queue must expose its hard-IRQ bridge")
        .1
        .split_once("\n    }\n}")
        .expect("IRQ bridge must remain a focused operation")
        .0;

    assert!(
        record.contains("record_irq_service_error(&error)"),
        "an acknowledged snapshot cannot be left behind a quenched action without recovery work"
    );
}

#[test]
fn recovery_owner_loss_is_fatal_instead_of_fabricating_an_offline_queue() {
    let service = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/service_loop.rs");
    let begin = service
        .split_once("fn begin_recovery")
        .expect("hctx must publish ordinary recovery through its owner link")
        .1
        .split_once("pub(super) fn record_irq_service_error")
        .expect("ordinary and IRQ recovery publication must remain separate")
        .0;
    assert!(begin.contains("assert!("));
    assert!(begin.contains("controller_link.request_recovery(cause)"));
    assert!(!begin.contains("mark_offline()"));

    let irq = service
        .split_once("pub(super) fn record_irq_service_error")
        .expect("hctx must publish IRQ recovery through its owner link")
        .1
        .split_once("pub(super) fn record_service_error")
        .expect("IRQ recovery publication must remain focused")
        .0;
    assert!(irq.contains("assert!("));
    assert!(irq.contains("controller_link.request_irq_recovery(self.info.id)"));
    assert!(!irq.contains("mark_offline()"));
}

#[test]
fn emergency_irq_mask_retains_actions_until_device_generation_is_masked() {
    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs");
    let start = controller
        .find("fn mask_recovery_sources")
        .expect("controller must expose one fail-closed IRQ mask helper");
    let end = controller[start..]
        .find("fn enable_recovery_irqs")
        .map(|offset| offset + start)
        .expect("mask helper must precede the re-enable helper");
    let helper = &controller[start..end];
    let disable_action = helper
        .find("registration.disable()")
        .expect("mask helper must close OS IRQ action admission");
    let disable_device = helper
        .find("device.lock().disable_irq()")
        .expect("mask helper must mask device IRQ delivery");

    assert!(
        disable_device < disable_action,
        "a device that cannot be masked must retain its live acknowledgement actions"
    );
}

#[test]
fn recovery_unmask_failure_remasks_device_before_disabling_actions() {
    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs");
    let helper = controller
        .split_once("fn enable_recovery_irqs")
        .expect("controller must expose one recovery IRQ activation helper")
        .1
        .split_once("fn recovery_input")
        .expect("IRQ activation and recovery input must remain separate")
        .0;
    let enable = helper
        .find("device.lock().enable_irq()")
        .expect("recovery must activate device interrupt generation");
    let remask = helper[enable..]
        .find("device.lock().disable_irq()")
        .map(|offset| offset + enable)
        .expect("a failed device activation must be explicitly remasked");
    let action_disable = helper[enable..]
        .find("registration.disable()")
        .map(|offset| offset + enable)
        .expect("failed activation must roll back OS actions");
    assert!(
        remask < action_disable,
        "OS acknowledgement actions must remain live until device remasking commits"
    );
}

#[test]
fn timeout_claim_cannot_publish_terminal_before_dma_ownership_returns() {
    let request = read_workspace_file("os/arceos/modules/axruntime/src/block/request.rs");
    assert!(
        request.contains("struct CompletionClaim") && request.contains("struct TimeoutClaim"),
        "completion and timeout must have distinct typed terminal claims"
    );
    let timeout_claim = request
        .split_once("struct TimeoutClaim")
        .expect("timeout claim type must exist")
        .1
        .split_once("pub(crate) struct RequestTagSet")
        .expect("claim types must precede the tag table")
        .0;
    assert!(
        !timeout_claim.contains("fn finish("),
        "watchdog ownership cannot publish terminal completion before quiescence returns DMA"
    );
    assert!(request.contains("finish_timeout_after_return"));
}

#[test]
fn accepted_timeout_claim_survives_service_work_admission_failure() {
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/mod.rs");
    let timeout = hctx
        .split_once("fn claim_timeout")
        .expect("hctx must expose the timeout ownership boundary")
        .1
        .split_once("fn request_cancel")
        .expect("timeout and cancellation paths must remain separate")
        .0;

    assert!(
        timeout.contains("record_service_error(&error)"),
        "once timeout wins, a work admission failure must enter controller recovery"
    );
    assert!(
        !timeout.contains("self.queue_service(HctxCause::Timeout)?"),
        "timeout ownership cannot be reported as an unaccepted operation"
    );
}

#[test]
fn hctx_faults_use_an_explicit_controller_owner_link() {
    let controller = rust_sources_under("os/arceos/modules/axruntime/src/block/controller");
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/mod.rs");

    assert!(
        controller.contains("struct ControllerOwnerLink"),
        "controller activation must create one stable owner link before hctx construction"
    );
    assert!(
        hctx.contains("controller_link: &'static ControllerOwnerLink"),
        "each hctx must retain its controller owner instead of searching a global registry"
    );
    assert!(
        !controller.contains("notify_hctx_fault") && !controller.contains("contains_hctx"),
        "fault handling must not scan the global controller registry"
    );
}

#[test]
fn fail_closed_quarantine_wakes_request_local_waiters() {
    let hctx = rust_sources_under("os/arceos/modules/axruntime/src/block/hctx");

    assert!(
        hctx.contains("notify_all_waiters_offline"),
        "offline quarantine must wake each request-local waiter"
    );
    assert!(
        hctx.contains("HctxPhase::Offline => Err(HardwareQueueError::Offline)"),
        "a quarantined DMA request must return Offline without fabricating ownership"
    );
}

#[test]
fn acknowledged_irq_is_serviced_before_watchdog_timeout() {
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/service_loop.rs");
    let service = hctx
        .split_once("fn service_bounded")
        .map(|(_, tail)| tail)
        .and_then(|tail| {
            tail.split_once("fn service_irq_events")
                .map(|(body, _)| body)
        })
        .expect("hctx must expose one bounded service callback");

    let irq = service
        .find("self.service_irq_events")
        .expect("bounded service must consume acknowledged IRQ snapshots");
    let timeout = service
        .find("HctxCause::Timeout")
        .expect("bounded service must arbitrate explicit timeout causes");
    let watchdog = service
        .find("self.service_watchdog")
        .expect("bounded service must inspect watchdog deadlines");

    assert!(
        irq < timeout && irq < watchdog,
        "an IRQ already acknowledged by the top half must win before timeout arbitration"
    );
}

#[test]
fn driver_service_error_cannot_drop_completions_already_returned_to_the_runtime() {
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/service_loop.rs");
    let service = hctx
        .split_once("fn service_irq_events")
        .expect("hctx must expose IRQ event service")
        .1
        .split_once("pub(super) fn publish_one_completion")
        .expect("IRQ service and completion publication must remain separate")
        .0;

    assert!(
        !service.contains("driver.service_events(&events, &mut completions)?"),
        "a driver error must not unwind through a batch that already owns returned requests"
    );
    let drain = service
        .find("completions.drain_with")
        .expect("every returned completion must be published or quarantined");
    let error_return = service
        .rfind("progress.map")
        .expect("driver service failure must be propagated after ownership drain");
    assert!(drain < error_return);
}

#[test]
fn one_irq_continuation_uses_one_callback_wide_service_budget() {
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/service_loop.rs");
    let service = hctx
        .split_once("fn service_irq_events")
        .expect("hctx must expose IRQ event service")
        .1
        .split_once("pub(super) fn publish_one_completion")
        .expect("IRQ service and completion publication must remain separate")
        .0;

    assert!(
        !service.contains("while") && service.matches(".service_events(").count() == 1,
        "the driver may return all 64 requests, so one callback may invoke it only once"
    );
    assert!(
        service.contains("let budget_result = consume_service_budget(budget, completed.max(1))"),
        "the IRQ budget must account for the larger of one service transition or returned \
         completions"
    );
    let ownership_drain = service
        .find("completions.drain_with")
        .expect("driver-returned ownership must be drained");
    let budget_propagation = service
        .find("budget_result?")
        .expect("budget failure must still propagate");
    assert!(
        ownership_drain < budget_propagation,
        "driver-returned ownership must be published or quarantined before a budget error returns"
    );
    assert!(
        service.contains("matches!(progress, Ok(ServiceProgress::More))"),
        "ServiceProgress::More must retain the exact IRQ evidence for a later callback"
    );
    assert!(
        service.contains("has_deferred_irq || !self.events.is_empty()"),
        "budget exhaustion must re-arm service for IRQ snapshots still present in the ring"
    );
}

#[test]
fn rejected_completion_ownership_has_a_proof_gated_fixed_quarantine() {
    let requests =
        read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/request_table.rs");
    let quarantine =
        read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/completion_quarantine.rs");
    let service = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/service_loop.rs");

    assert!(requests.contains("enum RequestOwnership"));
    assert!(requests.contains("struct DispatchPermit"));
    assert!(quarantine.contains("struct CompletionPublicationError"));
    assert!(quarantine.contains("struct RejectedCompletionQuarantine"));
    assert!(quarantine.contains("proof.controller_cookie() != self.controller_cookie"));
    assert!(quarantine.contains("proof.epoch() <= self.reclaimed_epoch"));
    let publication = requests
        .split_once("fn install_completion")
        .expect("request completion must have one ownership installation point")
        .1
        .split_once("fn completion_is_ready")
        .expect("completion installation and waiter observation must remain separate")
        .0;
    assert!(
        !publication.contains("mem::forget"),
        "request-table rejection must return ownership to the hctx quarantine"
    );
    assert!(service.contains("release_after_dma_quiesce(proof)"));
    assert!(service.contains("enter_fatal_completion_quarantine"));
    assert!(service.contains("fatal_completion_quarantine"));
    assert!(!service.contains("mem::forget(completion)"));

    let staging = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/staging.rs");
    assert!(staging.contains("overflow: Option<CompletedRequest>"));
    assert!(staging.contains("take_overflow"));
    assert!(!staging.contains("mem::forget(completion)"));
}

#[test]
fn recovery_reclaim_error_cannot_drop_partially_returned_request_ownership() {
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/service_loop.rs");
    let reclaim = hctx
        .split_once("pub(in crate::block) fn reclaim_after_quiesce")
        .expect("hctx recovery must reclaim requests after typed DMA quiescence")
        .1;

    assert!(
        !reclaim.contains(".reclaim_after_quiesce(proof, &mut completions)?"),
        "a partial driver reclaim error must not unwind through returned ownership"
    );
    let drain = reclaim
        .find("completions.drain_with")
        .expect("recovery must drain every completion returned before an error");
    let driver_result = reclaim
        .rfind("driver_result.map_err")
        .expect("the driver error must be propagated after ownership publication");
    assert!(drain < driver_result);
}

#[test]
fn accepted_direct_request_uses_recovery_when_watchdog_work_admission_fails() {
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/submission.rs");
    let finish = hctx
        .split_once("fn finish_submit_dispatch")
        .expect("direct dispatch must have one ownership-commit boundary")
        .1
        .split_once("fn dispatch_one_locked")
        .expect("dispatch completion handling must remain a focused function")
        .0;

    assert!(
        !finish.contains("accepted block request cannot activate its watchdog service"),
        "work admission failure is recoverable after the driver accepted ownership"
    );
    assert!(
        finish.contains("record_service_error(&error)"),
        "an accepted request must enter controller recovery instead of being abandoned or \
         panicking"
    );
}

#[test]
fn controller_recovery_waits_for_action_specific_irq_drain_events() {
    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");
    let recovery_irq =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs");
    let recovery = controller
        .split_once("fn recover_bounded")
        .map(|(_, tail)| tail)
        .and_then(|tail| {
            tail.split_once("fn abort_failed_activation")
                .map(|(body, _)| body)
        })
        .expect("controller must expose one bounded recovery callback");

    assert!(recovery_irq.contains("IrqDrainWake"));
    assert!(recovery.contains("disable_async"));
    assert!(recovery.contains("action_drain_complete"));
    let drain_actions = recovery
        .split_once("RecoveryStep::DrainActions =>")
        .expect("recovery must own a distinct action-drain state")
        .1
        .split_once("RecoveryStep::BeginQuiesce =>")
        .expect("action drain must finish before DMA quiescence")
        .0;
    assert!(
        !drain_actions.contains("is_synchronized")
            && !drain_actions.contains("WorkOutcome::Requeue"),
        "shared-worker recovery must sleep for a target-action drain wake, not poll a whole IRQ \
         descriptor"
    );
}

#[test]
fn recovery_irq_wake_closes_the_poll_to_schedule_publication_window() {
    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs");
    let record = controller
        .split_once("pub(super) fn record_lifecycle_irq")
        .expect("controller recovery must expose its lifecycle IRQ bridge")
        .1
        .split_once("fn irq_drain_wake")
        .expect("lifecycle IRQ publication must remain a focused operation")
        .0;
    let arm = controller
        .split_once("fn arm_recovery_schedule")
        .expect("controller recovery must publish one initialization schedule")
        .1;

    let pending_publish = record
        .find("recovery_pending_sources")
        .expect("IRQ must latch acknowledged lifecycle evidence");
    let waiting_recheck = record
        .rfind("recovery_wait_sources")
        .expect("IRQ must recheck the worker's published wait mask");
    assert!(
        pending_publish < waiting_recheck && record.contains("fence(Ordering::SeqCst)"),
        "IRQ must publish evidence before the symmetric wait-mask recheck"
    );
    assert!(
        arm.contains("fence(Ordering::SeqCst)")
            && arm.contains("recovery_pending_sources.load(Ordering::Acquire)"),
        "the worker must recheck early IRQ evidence after publishing its wait mask"
    );
}

#[test]
fn recovery_clears_old_irq_evidence_before_publishing_the_new_phase() {
    let recovery =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");
    let schedule = recovery
        .split_once("pub(super) fn schedule_recovery")
        .expect("controller must expose one recovery admission transition")
        .1
        .split_once("fn recovery_work")
        .expect("recovery admission must remain separate from work accessors")
        .0;

    let serialized = schedule
        .find("let mut recovery_cause = self.recovery_cause.lock()")
        .expect("concurrent recovery requests must serialize initialization");
    let clear_pending = schedule
        .find("self.recovery_pending_sources.store(0")
        .expect("a new recovery epoch must discard old acknowledged evidence");
    let publish_recovering = schedule
        .find("self.phase.compare_exchange_weak")
        .expect("recovery phase publication must be an atomic transition");
    assert!(serialized < clear_pending && clear_pending < publish_recovering);
}

#[test]
fn controller_recovery_requires_typed_dma_and_ready_proofs() {
    let controller =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");
    let recovery = controller
        .split_once("fn recover_bounded")
        .map(|(_, tail)| tail)
        .and_then(|tail| {
            tail.split_once("fn abort_failed_activation")
                .map(|(body, _)| body)
        })
        .expect("controller must expose one bounded recovery callback");

    for operation in [
        "begin_dma_quiesce",
        "poll_dma_quiesce",
        "begin_reinitialize",
        "poll_reinitialize",
    ] {
        assert!(
            recovery.contains(operation),
            "controller recovery is missing typed lifecycle operation {operation}"
        );
    }
    assert!(controller.contains("validate_dma_proof(&self, proof: &DmaQuiesced)"));
    assert!(controller.contains("validate_ready_proof(&self, proof: &ControllerReady)"));
}
