use std::{
    fs,
    path::{Path, PathBuf},
};

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
    fn append_sources(directory: &Path, output: &mut String) {
        let mut entries = fs::read_dir(directory)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
            .map(|entry| {
                entry
                    .expect("source directory entry must be readable")
                    .path()
            })
            .collect::<Vec<_>>();
        entries.sort();

        for path in entries {
            if path.is_dir() {
                append_sources(&path, output);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                output.push_str(
                    &fs::read_to_string(&path).unwrap_or_else(|error| {
                        panic!("failed to read {}: {error}", path.display())
                    }),
                );
            }
        }
    }

    let mut source = String::new();
    append_sources(&workspace_root().join(path), &mut source);
    source
}

fn rust_crate_sources_under(path: &str) -> String {
    let directory = workspace_root().join(path);
    let mut crates = fs::read_dir(&directory)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", directory.display()))
        .map(|entry| {
            entry
                .expect("crate directory entry must be readable")
                .path()
        })
        .collect::<Vec<_>>();
    crates.sort();

    let mut sources = String::new();
    for crate_path in crates {
        let source_path = crate_path.join("src");
        if !source_path.is_dir() {
            continue;
        }
        let relative = source_path
            .strip_prefix(workspace_root())
            .expect("driver source directory must remain inside the workspace")
            .to_str()
            .expect("workspace driver source path must be UTF-8");
        sources.push_str(&rust_sources_under(relative));
    }
    sources
}

fn source_section<'source>(source: &'source str, start: &str, end: &str) -> &'source str {
    let start_offset = source
        .find(start)
        .unwrap_or_else(|| panic!("missing source-section start marker `{start}`"));
    let remaining = &source[start_offset..];
    let end_offset = remaining
        .find(end)
        .unwrap_or_else(|| panic!("missing source-section end marker `{end}` after `{start}`"));
    &remaining[..end_offset]
}

fn assert_in_order(source: &str, markers: &[&str]) {
    let mut cursor = 0;
    for marker in markers {
        let offset = source[cursor..]
            .find(marker)
            .unwrap_or_else(|| panic!("missing ordered marker `{marker}`"));
        cursor += offset + marker.len();
    }
}

fn assert_absent(source: &str, forbidden: &[&str], scope: &str) {
    for symbol in forbidden {
        assert!(
            !source.contains(symbol),
            "{scope} still contains forbidden path `{symbol}`"
        );
    }
}

fn without_whitespace(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

#[test]
fn block_feature_selects_maintenance_without_device_workqueue() {
    let manifest = read_workspace_file("os/arceos/modules/axruntime/Cargo.toml");
    let block_feature = source_section(&manifest, "block = [", "]\nfp-simd");

    assert!(block_feature.contains("\"maintenance\""));
    assert!(block_feature.contains("\"ax-driver/block\""));
    assert!(
        !block_feature.contains("workqueue"),
        "block devices have dedicated maintenance owners, not shared workqueue callbacks"
    );
}

#[test]
fn filesystem_and_rdif_keep_the_irq_only_boundary() {
    let fs_manifest = read_workspace_file("os/arceos/modules/axfs-ng/Cargo.toml");
    let fs_sources = rust_sources_under("os/arceos/modules/axfs-ng/src");
    let rdif_sources = rust_sources_under("drivers/interface/rdif-block/src");

    assert_absent(
        &fs_manifest,
        &["rdif-block", "dma-api", "irq-framework", "ax-runtime"],
        "ax-fs-ng manifest",
    );
    assert_absent(
        &fs_sources,
        &[
            "BlockCompletionMode",
            "RequestPoller",
            "poll_request",
            "poll_completions",
            "RequestFlags::POLLED",
            "irq_driven",
        ],
        "ax-fs-ng block boundary",
    );
    assert_absent(
        &rdif_sources,
        &[
            "BlockCompletionMode",
            "RequestPoller",
            "poll_request",
            "poll_completions",
            "RequestFlags::POLLED",
            "DispatchMode::Direct",
        ],
        "rdif-block interface",
    );
    assert!(rdif_sources.contains("QueueKind::Interrupt"));
    assert!(rdif_sources.contains("QueueExecution::Tagged"));
    assert!(rdif_sources.contains("QueueExecution::Serialized"));
}

#[test]
fn request_watchdog_policy_is_owned_by_the_runtime() {
    let runtime = rust_sources_under("os/arceos/modules/axruntime/src/block");
    let activation = read_workspace_file("drivers/interface/rdif-block/src/activation/mod.rs");

    assert!(runtime.contains("pub struct BlockRuntimeConfig"));
    assert!(runtime.contains("DEFAULT_REQUEST_WATCHDOG_NS"));
    assert!(
        !runtime.contains("limits.request_timeout_ns"),
        "portable queue metadata must not select the OS watchdog policy"
    );

    let hardware_limits = source_section(
        &activation,
        "pub struct HardwareQueueLimits",
        "impl HardwareQueueLimits",
    );
    assert!(
        !hardware_limits.contains("request_timeout_ns"),
        "v0.13 hardware limits describe hardware, not runtime watchdog policy"
    );
}

#[test]
fn queue_shutdown_cannot_publish_request_ownership() {
    let mut implementations = rust_crate_sources_under("drivers/blk");
    implementations.push_str(&rust_sources_under("drivers/ax-driver/src/block"));
    implementations.push_str(&rust_sources_under("drivers/ax-driver/src/virtio/block"));
    implementations.push_str(&rust_sources_under("os/arceos/modules/axruntime/src/block"));
    let implementations = without_whitespace(&implementations);

    assert!(
        !implementations.contains("fnshutdown(&mutself,"),
        "queue shutdown must not retain a completion sink or any ownership-return channel"
    );
}

#[test]
fn maintenance_thread_pins_itself_before_registering_irq_actions() {
    let runtime = read_workspace_file("os/arceos/modules/axruntime/src/maintenance/runtime.rs");
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let source = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/source.rs");
    let irq = read_workspace_file("os/arceos/modules/axruntime/src/irq.rs");

    let current = source_section(
        &runtime,
        "pub fn run_maintenance_current",
        "pub fn spawn_maintenance_domain",
    );
    assert_in_order(
        current,
        &[
            "let cpu_lease = pin_current_cpu()?",
            "let thread = current_thread_handle()?",
            "owner_cpu",
            "owner_thread: thread.id()",
            "MaintenanceRegistrar",
        ],
    );

    let spawned = source_section(
        &runtime,
        "pub fn spawn_maintenance_domain",
        "fn classify_task_wake",
    );
    assert_in_order(
        spawned,
        &[
            "let mut affinity = CpuSet::empty(topology)",
            "affinity.insert(CpuId::new(cpu_id))",
            "spawn_kernel_worker",
            "run_maintenance_current",
        ],
    );

    let controller_spawn = source_section(
        &activation,
        "pub(super) fn activate_controller",
        "fn run_controller_owner",
    );
    assert!(controller_spawn.contains("spawn_maintenance_domain"));
    assert!(controller_spawn.contains("move |registrar|"));
    assert!(controller_spawn.contains("run_controller_owner(device, topology, config, registrar"));

    let initialize = activation
        .split_once("pub(super) fn initialize_controller_on_owner")
        .expect("controller initialization entry must remain in activation orchestration")
        .1;
    assert_in_order(
        initialize,
        &[
            "register_initial_sources",
            "registrar.remote_handle()",
            "registrar.activate()?",
            "enable_irq_delivery",
        ],
    );
    assert!(source.contains("registrar.register_shared_disabled"));
    assert!(source.contains("Active(MaintenanceIrqAction)"));
    assert!(
        !source.contains("Registration::register_shared_disabled_on"),
        "block IRQ actions must be created through the owner-validating maintenance capability"
    );
    assert!(irq.contains("IrqAffinity::Fixed(CpuId(cpu))"));
    assert!(irq.contains(".auto_enable(AutoEnable::No)"));
}

#[test]
fn owner_capabilities_revalidate_thread_and_cpu_identity() {
    let runtime = read_workspace_file("os/arceos/modules/axruntime/src/maintenance/runtime.rs");

    let registrar = source_section(
        &runtime,
        "pub struct MaintenanceRegistrar",
        "impl<T: Copy + Send + 'static> MaintenanceRegistrar",
    );
    let session = source_section(
        &runtime,
        "pub struct MaintenanceSession",
        "impl<T: Copy + Send + 'static> MaintenanceSession",
    );
    assert!(!registrar.contains("CurrentCpuLease"));
    assert!(registrar.contains("PhantomData<*mut ()>"));
    assert!(!session.contains("CurrentCpuLease"));
    assert!(session.contains("PhantomData<*mut ()>"));
    let runner = source_section(
        &runtime,
        "pub fn run_maintenance_current",
        "/// Creates one fair",
    );
    assert!(runner.contains("let cpu_lease = pin_current_cpu()?"));
    assert!(runner.contains("quarantine_owner_forever(core, cpu_lease"));

    let validate = source_section(
        &runtime,
        "pub(super) fn validate_owner_identity",
        "impl<T: Copy + Send + 'static> Drop for MaintenanceSession",
    );
    assert_in_order(
        validate,
        &[
            "this_cpu_id_pinned",
            "owner_cpu",
            "current_thread_id()?",
            "owner_thread",
        ],
    );

    let local_wake = source_section(
        &runtime,
        "pub fn publish_from_irq",
        "pub fn owner_cpu(&self)",
    );
    assert_in_order(
        local_wake,
        &[
            "in_irq_context()",
            "this_cpu_id()",
            "self.core.owner_cpu",
            "self.wake.thread_id()",
            "self.core.owner_thread",
            "publish_irq_event_serialized",
            "self.wake.wake()",
        ],
    );
    assert_absent(
        local_wake,
        &["send_ipi", "queue_work_on", "Arc::clone", "spawn_"],
        "LocalIrqWake hard-IRQ path",
    );
}

#[test]
fn block_irq_callback_only_captures_publishes_and_contains() {
    let source = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/source.rs");
    let callback = source_section(&source, "move |context| {", "})?;");

    assert_in_order(
        callback,
        &[
            "context.cpu.0 != owner_cpu",
            "endpoint.capture()",
            "IrqCapture::Captured",
            "wake.publish_from_irq(MaintenanceCauses::IRQ, event)",
        ],
    );
    assert!(callback.contains("IrqCapture::Unhandled"));
    assert!(callback.contains("IrqCapture::Fault"));
    assert!(callback.contains("DisableActionAndWake"));
    assert_absent(
        callback,
        &[
            "service_owner_queues",
            "service_events",
            "submit_owned",
            "publish_one_completion",
            "rearm(",
            "queue.lock",
            "device.lock",
            "queue_work_on",
            "Box::new",
            "Vec::",
        ],
        "block hard-IRQ callback",
    );

    let rdif_irq = read_workspace_file("drivers/interface/rdif-irq/src/lib.rs");
    assert!(rdif_irq.contains("fn capture(&mut self) -> IrqCapture"));
    assert!(rdif_irq.contains("fn contain(&mut self, cause: ContainmentCause)"));
    assert!(rdif_irq.contains("pub trait IrqSourceControl"));
    assert!(rdif_irq.contains("generation: NonZeroU64"));
    assert!(rdif_irq.contains("bitmap: NonZeroU64"));
    assert_absent(
        &rdif_irq,
        &[
            "Deferred",
            "Continuation",
            "Waker",
            "ThreadWakeHandle",
            "ax_task",
        ],
        "portable IRQ endpoint",
    );
}

#[test]
fn every_hardware_submit_stages_before_owner_dispatch() {
    let submission =
        read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/submission.rs");
    let service = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/service_loop.rs");
    let info = read_workspace_file("drivers/interface/rdif-block/src/info.rs");
    let interface = read_workspace_file("drivers/interface/rdif-block/src/interface.rs");

    let public_submit = source_section(
        &submission,
        "pub fn submit_owned",
        "fn stage_on_current_cpu",
    );
    assert!(public_submit.contains("self.stage_on_current_cpu(tag)"));
    assert_absent(
        public_submit,
        &["driver.submit_owned", "queue.lock()", "service_events"],
        "request-thread submission",
    );

    let staging = source_section(
        &submission,
        "fn stage_on_current_cpu",
        "pub(super) fn dispatch_one_locked",
    );
    assert_in_order(
        staging,
        &[
            "self.requests.ensure_staged(tag)?",
            ".stage(cpu, self.hctx_index, tag)?",
            "self.queue_service(HctxCause::Submit)",
        ],
    );
    assert_absent(
        staging,
        &["driver.submit_owned", "service_events"],
        "software staging",
    );

    let owner_dispatch = source_section(&submission, "pub(super) fn dispatch_one_locked", "\n}\n");
    assert!(owner_dispatch.contains("driver.submit_owned(id, request)"));
    let staged_dispatch = source_section(&service, "fn dispatch_staged", "fn next_dispatch_tag");
    assert_in_order(
        staged_dispatch,
        &["self.dispatch_one_locked(", "&mut driver"],
    );

    assert!(!info.contains("Direct"));
    assert!(interface.contains(
        "(QueueKind::Interrupt { .. }, QueueExecution::Tagged | QueueExecution::Serialized)"
    ));
}

#[test]
fn owner_loop_preserves_irq_terminal_and_dispatch_order() {
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let service = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/service_loop.rs");

    let owner_loop = source_section(&activation, "fn run_owner_loop", "fn rearm_runtime_sources");
    assert_in_order(
        owner_loop,
        &[
            "session.drain_owner",
            "controller.route_owner_irq",
            "controller.service_owner_queues()",
            "rearm_runtime_sources",
            "controller.service_owner_return",
            "controller.service_owner_recovery",
            "controller.service_owner_handoff",
        ],
    );
    assert!(owner_loop.contains("MAINTENANCE_BATCH_LIMIT"));
    assert!(owner_loop.contains("yield_current_cpu"));

    let queue_pass = source_section(&service, "fn service_bounded", "fn defer_after_irq_budget");
    assert_in_order(
        queue_pass,
        &[
            "self.service_irq_events(&mut budget)?",
            "HctxCause::EventOverflow",
            "HctxCause::Timeout",
            "HctxCause::Watchdog",
            "self.service_watchdog",
            "HctxCause::Cancel",
            "self.service_cancellations",
            "self.dispatch_staged(&mut budget)?",
        ],
    );
    assert!(queue_pass.contains("ServiceBudget::new(HCTX_SERVICE_BUDGET)"));
}

#[test]
fn handoff_and_recovery_are_bounded_owner_state_machines() {
    let handoff =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/handoff_owner.rs");
    let recovery =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");

    assert!(handoff.contains("pub(in crate::block) enum OwnerHandoff"));
    assert!(handoff.contains("pub(in crate::block) fn service_owner_handoff"));
    assert!(handoff.contains("match handoff"));
    assert!(handoff.contains("InitPoll::Pending(schedule)"));
    assert!(recovery.contains("pub(super) enum RecoveryStep"));
    assert!(recovery.contains("pub(in crate::block) fn service_owner_recovery"));
    assert!(recovery.contains("for _ in 0..RECOVERY_TRANSITION_BUDGET"));
    assert!(recovery.contains("InitPoll::Pending(schedule)"));

    for (scope, source) in [
        ("handoff", handoff.as_str()),
        ("recovery", recovery.as_str()),
    ] {
        assert_absent(
            source,
            &[
                "wait_for_pending(",
                "wait_for_pending_until(",
                "sleep_until(",
                ".park(",
                "block_on(",
                "queue_work_on(",
                "WorkQueue",
            ],
            scope,
        );
    }
}

#[test]
fn handoff_retains_and_reattaches_the_same_owner_actions() {
    let routes =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/irq_routes.rs");
    let source = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/source.rs");
    let recovery =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/recovery.rs");

    let detach = source_section(&routes, "pub(super) fn detach_host_actions", "/// Restores");
    assert_in_order(
        detach,
        &[
            "controller.with_driver_endpoint_on_owner(|device| device.disable_irq())?",
            "quiesce_after_device_masked(sources)?",
            "source.detach()?",
        ],
    );
    let quiesce = source_section(
        &source,
        "pub(in crate::block) fn quiesce_after_device_masked",
        "impl Drop for RuntimeIrqSource",
    );
    assert_in_order(
        quiesce,
        &[
            "source.disable()?",
            "source.release_quench()?",
            "source.synchronize()?",
        ],
    );
    assert!(source.contains("enum RuntimeIrqAction"));
    assert!(source.contains("Active(MaintenanceIrqAction)"));
    assert!(source.contains("Detached(MaintenanceDetachedIrqAction)"));
    assert!(source.contains("detached.reattach()"));

    let guest_return = source_section(
        &recovery,
        "fn begin_return_from_guest",
        "pub(super) fn return_from_guest",
    );
    assert_in_order(
        guest_return,
        &[
            "reattach_host_actions(sources)?",
            "queue.begin_guest_return_recovery()?",
            "self.advance_recovery_epoch()?",
            "RecoveryStep::DisableActions",
        ],
    );
}

#[test]
fn explicit_close_precedes_reclamation_and_failed_drop_is_quarantined() {
    let teardown =
        read_workspace_file("os/arceos/modules/axruntime/src/block/activation/teardown.rs");
    let runtime = read_workspace_file("os/arceos/modules/axruntime/src/maintenance/runtime.rs");
    let lifecycle = read_workspace_file("os/arceos/modules/axruntime/src/maintenance/lifecycle.rs");
    let quarantine = read_workspace_file("os/arceos/modules/axruntime/src/block/quarantine.rs");
    let logical_devices =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/device.rs");
    let hctx_lifecycle =
        read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/lifecycle.rs");
    let irq_source =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/source.rs");
    let irq_source_quarantine = read_workspace_file(
        "os/arceos/modules/axruntime/src/block/controller/source/quarantine.rs",
    );
    let mut owned_runtime = rust_sources_under("os/arceos/modules/axruntime/src/block");
    owned_runtime.push_str(&rust_sources_under(
        "os/arceos/modules/axruntime/src/maintenance",
    ));

    let close = teardown
        .split_once("pub(super) fn close_controller_resources")
        .expect("block controller close must remain an explicit teardown transaction")
        .1;
    assert_in_order(
        close,
        &[
            "session.begin_close()",
            "controller.disable_device_irq_on_owner()",
            "quiesce_after_device_masked(&sources)",
            "close_irq_sources(sources)",
            "source.close()",
            "session.try_begin_draining()",
            ".drain_owner(",
            "session.finish_close()",
            ".try_into_closed()",
        ],
    );

    assert!(lifecycle.contains("MaintenanceState::Quarantined"));
    assert!(lifecycle.contains("IrqCapabilitiesLive"));
    assert!(lifecycle.contains("PublishersActive"));
    assert!(lifecycle.contains("MailboxPending"));
    let session_drop = source_section(
        &runtime,
        "impl<T: Copy + Send + 'static> Drop for MaintenanceSession",
        "/// Failed close conversion",
    );
    assert!(session_drop.contains("core.lifecycle.quarantine()"));

    assert!(quarantine.contains("const QUEUE_QUARANTINE_CAPACITY"));
    assert!(quarantine.contains("QueueQuarantineRegistry"));
    assert!(quarantine.contains("queue.close()"));
    assert!(quarantine.contains("failure.into_quarantine()"));
    assert!(logical_devices.contains("quarantine_live_queue(queue, reason, reservation)"));
    assert!(hctx_lifecycle.contains("quarantine_live_queue(queue, reason, reservation)"));
    assert!(irq_source_quarantine.contains("IrqSourceQuarantineRegistry"));
    assert!(irq_source.contains("fn close(mut self)"));
    assert!(irq_source_quarantine.contains("pub(super) fn retain"));
    assert_absent(
        &(logical_devices + &hctx_lifecycle),
        &["ManuallyDrop"],
        "runtime queue owners with pre-reserved quarantine capacity",
    );
    assert_absent(
        &owned_runtime,
        &["Box::leak", "mem::forget"],
        "maintenance and block ownership",
    );
}

#[test]
fn hctx_completion_ownership_uses_drop_or_named_quarantine_without_anonymous_leaks() {
    let hctx = rust_sources_under("os/arceos/modules/axruntime/src/block/hctx");
    let hctx_owner = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/mod.rs");
    let lifecycle = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/lifecycle.rs");
    let quarantine =
        read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/completion_quarantine.rs");
    let staging = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/staging.rs");

    assert_absent(
        &hctx,
        &["ManuallyDrop", "mem::forget", "Box::leak"],
        "hctx request and completion ownership",
    );
    assert!(quarantine.contains("CompletionQuarantineReservation"));
    assert!(quarantine.contains("CompletionQuarantineRegistry"));
    assert!(quarantine.contains("fn retain(self, quarantine: Box<RejectedCompletionQuarantine>)"));
    assert!(staging.contains("impl Drop for DeferredCompletionSink"));
    assert!(staging.contains("self.finish_notifications()"));
    assert_in_order(
        &hctx_owner,
        &[
            "CompletionQuarantineReservation::reserve",
            "Ok(Arc::new(Self",
        ],
    );
    let drop = source_section(
        &lifecycle,
        "impl Drop for HardwareQueue",
        "pub(super) fn shutdown_unpublished_queue",
    );
    assert!(drop.contains("reservation.retain(quarantine)"));
    assert!(drop.contains("reservation.release()"));
}

#[test]
fn owner_driver_transactions_exclude_the_same_cpu_irq_endpoint() {
    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let queue = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/ownership.rs");
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let initialization =
        read_workspace_file("os/arceos/modules/axruntime/src/block/activation/initialization.rs");
    let irq_source =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/source.rs");

    let controller_lease = source_section(
        &controller,
        "struct DriverEndpointLease<'slot, T>",
        "/// Shutdown-lifetime owner",
    );
    let queue_lease = source_section(
        &queue,
        "pub(super) struct DriverEndpointLease<'queue>",
        "impl Drop for DriverAccessGuard",
    );

    assert!(controller_lease.contains("IrqGuard"));
    assert!(queue_lease.contains("IrqGuard"));
    assert!(controller_lease.contains("drop(self.irq_guard.take())"));
    assert!(queue_lease.contains("drop(self.irq_guard.take())"));
    assert!(activation.contains("fn with_owner_irq_excluded"));
    assert!(
        initialization
            .contains("with_owner_irq_excluded(|| match device.bundle_mut().controller_init()")
    );
    let rearm = source_section(
        &irq_source,
        "pub(in crate::block) fn rearm_retained(",
        "pub(in crate::block) fn detach(",
    );
    assert_in_order(
        rearm,
        &[
            "let irq_guard = ax_kspin::IrqGuard::new()",
            "control.rearm(masked)",
            "drop(irq_guard)",
        ],
    );
}

#[test]
fn init_irq_replacement_quench_and_rearm_are_linear_owner_protocols() {
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");
    let irq_source =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/source.rs");

    let retire = source_section(
        &activation,
        "fn retire_initial_sources",
        "fn bind_normal_sources",
    );
    assert_in_order(
        retire,
        &[
            "owner.device.disable_irq()",
            "quiesce_after_device_masked(&owner.sources)",
            "core::mem::take(&mut owner.sources)",
            "close_irq_sources(initial_sources)",
            "drain_retired_initial_events(owner)",
        ],
    );
    assert!(!retire.contains("owner.sources.clear()"));

    let owner_activation = source_section(
        &activation,
        "fn run_controller_owner",
        "fn retire_initial_sources",
    );
    assert_in_order(
        owner_activation,
        &[
            "retire_initial_sources",
            "BlockController::prepare_on_owner",
            "bind_normal_sources",
            "prepared.commit_on_owner",
        ],
    );

    let teardown = source_section(
        &irq_source,
        "pub(in crate::block) fn quiesce_after_device_masked",
        "impl Drop for RuntimeIrqSource",
    );
    assert_in_order(
        teardown,
        &[
            "source.disable()",
            "source.release_quench()",
            "source.synchronize()",
        ],
    );

    let rearm = source_section(
        &irq_source,
        "pub(in crate::block) fn rearm_retained(",
        "pub(in crate::block) fn detach(",
    );
    assert_in_order(rearm, &[".enable()", ".rearm(masked)"]);

    let registration = source_section(
        &irq_source,
        "fn register_disabled_with",
        "pub(in crate::block) fn register_initial_disabled",
    );
    assert!(registration.contains("source_id >= u64::BITS as usize"));
    assert!(irq_source.contains("_not_send: PhantomData<*mut ()>"));
}

#[test]
fn obsolete_device_progression_paths_cannot_reappear() {
    let block = rust_sources_under("os/arceos/modules/axruntime/src/block");

    assert_absent(
        &block,
        &[
            "IrqContinuation",
            "DeferredIrqProgress",
            "InitIrqProgress",
            "service_deferred_irq",
            "continue_deferred_irq",
            "record_deferred_irq",
            "queue_work_on(",
            "WorkQueue::new",
            "DispatchMode::Direct",
            "run_on_cpu_sync_raw",
            "poll_request",
            "poll_completions",
            "read_volatile",
            "write_volatile",
            "ack_interrupt",
            "interrupt_status",
        ],
        "ax-runtime block implementation",
    );
    assert!(
        !workspace_root()
            .join("os/arceos/modules/axruntime/src/block/controller/recovery_irq.rs")
            .exists(),
        "recovery IRQ handling belongs to the maintenance owner, not a second worker"
    );
}
