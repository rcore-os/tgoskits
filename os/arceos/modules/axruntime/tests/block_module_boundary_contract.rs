use std::{fs, path::PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(4)
        .expect("ax-runtime must live under os/arceos/modules")
        .to_path_buf()
}

fn rust_source_paths_under(path: &str) -> Vec<PathBuf> {
    let mut pending = vec![workspace_root().join(path)];
    let mut sources = Vec::new();

    while let Some(path) = pending.pop() {
        for entry in fs::read_dir(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()))
        {
            let path = entry
                .expect("block runtime source entry must be readable")
                .path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                sources.push(path);
            }
        }
    }
    sources
}

fn read_workspace_file(path: &str) -> String {
    fs::read_to_string(workspace_root().join(path))
        .unwrap_or_else(|error| panic!("failed to read {path}: {error}"))
}

fn without_whitespace(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
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

#[test]
fn block_runtime_source_files_remain_domain_focused() {
    for path in rust_source_paths_under("os/arceos/modules/axruntime/src/block") {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        let line_count = source.lines().count();
        assert!(
            line_count <= 800,
            "{} has {line_count} lines; split block runtime code by owned invariant",
            path.display()
        );
    }
}

#[test]
fn block_maintenance_paths_do_not_hold_irq_off_locks() {
    for path in rust_source_paths_under("os/arceos/modules/axruntime/src/block") {
        let source = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        assert!(
            !source.contains("SpinNoIrq"),
            "{} uses an IRQ-off lock even though hard IRQ only publishes preallocated events",
            path.display()
        );
    }
}

#[test]
fn inline_queue_io_does_not_disable_interrupts_while_copying_memory() {
    let registry =
        workspace_root().join("os/arceos/modules/axruntime/src/block/controller/registry.rs");
    let controller = fs::read_to_string(&registry)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", registry.display()));
    let device_path =
        workspace_root().join("os/arceos/modules/axruntime/src/block/controller/device.rs");
    let device = fs::read_to_string(&device_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", device_path.display()));
    assert!(
        !controller.contains("Vec<rdif_block::CompletedRequest>")
            && !device.contains("Vec<rdif_block::CompletedRequest>"),
        "activation rollback completion callbacks must use fixed storage"
    );
    let inline_queue = device
        .split_once("struct InlineQueue")
        .expect("logical-device module must define the inline queue owner")
        .1
        .split_once("impl InlineQueue")
        .expect("inline queue representation must precede its constructor")
        .0;

    assert!(
        inline_queue.contains("SpinNoPreempt<Option<QueueHandle>>"),
        "inline memory I/O needs migration exclusion, not a long IRQ-off critical section"
    );
    assert!(!inline_queue.contains("SpinNoIrq<Option<QueueHandle>>"));

    let service_path = workspace_root().join("os/arceos/modules/axruntime/src/block/service.rs");
    let service = fs::read_to_string(&service_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", service_path.display()));
    let submit = service
        .split_once("fn submit_inline(")
        .expect("block service must keep one inline submission entry")
        .1
        .split_once("fn build_data_request(")
        .expect("inline submission must remain a focused operation")
        .0;
    assert!(submit.contains("queue.queue.lock()"));
    assert!(!submit.contains("IrqGuard") && !submit.contains("disable_irq"));
    assert!(
        submit.contains("drop(driver);\n    match outcome"),
        "returned request ownership must outlive the inline queue critical section"
    );
    assert!(
        !service.contains("alloc::vec::Vec<CompletedRequest>"),
        "inline invariant recovery must not allocate from a completion callback"
    );

    let rollback = controller
        .split_once("fn rollback_unpublished_runtime_queues")
        .expect("controller activation must own unpublished queue rollback")
        .1
        .split_once("fn shutdown_queue_iter")
        .expect("queue rollback must remain separate from IRQ registration")
        .0;
    assert!(
        rollback.contains("rollback_unpublished_runtime_queue(queue)"),
        "unpublished queues must converge on one typed rollback boundary"
    );
    assert!(device.contains("let queue = self.queue.lock().take()"));
    assert!(device.contains("close_or_quarantine(queue"));
    let drop_impl = device
        .split_once("impl Drop for InlineQueue")
        .expect("inline queue must return driver ownership on final drop")
        .1
        .split_once("impl BlockController")
        .expect("inline drop must precede logical-device publication")
        .0;
    assert!(
        drop_impl.contains(".get_mut().take()")
            && !drop_impl.contains(".shutdown(")
            && !drop_impl.contains(".lock()"),
        "final drop must retain exclusive ownership without re-entering driver shutdown"
    );
}

#[test]
fn inline_contract_violation_permanently_closes_future_submission() {
    let device_path =
        workspace_root().join("os/arceos/modules/axruntime/src/block/controller/device.rs");
    let device = fs::read_to_string(&device_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", device_path.display()));
    let service_path = workspace_root().join("os/arceos/modules/axruntime/src/block/service.rs");
    let service = fs::read_to_string(&service_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", service_path.display()));

    assert!(device.contains("available: AtomicBool"));
    let submit = service
        .split_once("fn submit_inline(")
        .expect("inline queues must have one focused submission path")
        .1
        .split_once("fn build_data_request")
        .expect("inline submission must remain separate from DMA construction")
        .0;
    let driver_lock = submit
        .find("let mut driver = queue.queue.lock()")
        .expect("inline submission must serialize driver ownership");
    let availability_recheck = submit[driver_lock..]
        .find("queue.available.load(Ordering::Acquire)")
        .map(|offset| offset + driver_lock)
        .expect("availability must be rechecked after acquiring the driver gate");
    assert!(driver_lock < availability_recheck);
    assert_eq!(
        submit.matches("queue.available.store(false").count(),
        1,
        "the driver gate must publish one permanent poison transition"
    );
    assert_eq!(
        submit
            .matches("contain_inline_contract_violation(queue)")
            .count(),
        3,
        "every malformed ownership outcome must enter the same one-shot shutdown transaction"
    );
}

#[test]
fn submit_rejection_returns_request_without_allocating_an_error_wrapper() {
    let path = workspace_root().join("os/arceos/modules/axruntime/src/block/hctx/mod.rs");
    let source = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
    let error = source
        .split_once("pub struct RuntimeSubmitError")
        .expect("hctx must expose the ownership-returning submit error")
        .1
        .split_once("impl RuntimeSubmitError")
        .expect("submit error representation must precede its methods")
        .0;

    assert!(error.contains("request: OwnedRequest"));
    assert!(
        !error.contains("Box<OwnedRequest>"),
        "queue admission failure must not turn recoverable backpressure into an OOM path"
    );
}

#[test]
fn irq_event_publication_snapshots_service_phase_and_epoch_atomically() {
    let hctx_path =
        workspace_root().join("os/arceos/modules/axruntime/src/block/hctx/irq_publication.rs");
    let hctx = fs::read_to_string(&hctx_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", hctx_path.display()));
    let record_irq = hctx
        .split_once("fn record_owner_irq_event")
        .expect("hardware queue must expose IRQ event publication")
        .1
        .split_once("\n    }\n}")
        .expect("IRQ publication must remain a focused operation")
        .0;
    let model_path = workspace_root().join("os/arceos/modules/axruntime/src/block/hctx_model.rs");
    let model = fs::read_to_string(&model_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", model_path.display()));

    assert!(
        model.contains("pub fn accepted_event_epoch(&self) -> Option<u64>"),
        "the lifecycle model must return phase and epoch from one atomic snapshot"
    );
    assert!(
        record_irq.contains("let Some(epoch) = queue.control.accepted_event_epoch()"),
        "hard IRQ must tag an event with the same snapshot that accepted it"
    );
    assert!(
        !record_irq.contains("services_accepted_work()")
            && !record_irq.contains("queue.control.epoch()"),
        "split phase/epoch reads can relabel a stale IRQ with the recovery epoch"
    );
}

#[test]
fn quarantine_capacity_is_reserved_before_queue_publication_and_normal_irq_enable() {
    let quarantine = read_workspace_file("os/arceos/modules/axruntime/src/block/quarantine.rs");
    let registry =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/registry.rs");
    let prepared =
        read_workspace_file("os/arceos/modules/axruntime/src/block/controller/prepared.rs");
    let device = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/device.rs");
    let hctx = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/mod.rs");
    let lifecycle = read_workspace_file("os/arceos/modules/axruntime/src/block/hctx/lifecycle.rs");
    let activation = read_workspace_file("os/arceos/modules/axruntime/src/block/activation.rs");

    assert!(quarantine.contains("struct QueueQuarantineReservations"));
    assert!(quarantine.contains("struct QueueQuarantineReservation"));
    assert!(quarantine.contains("impl Drop for QueueQuarantineReservations"));
    assert!(quarantine.contains("fn release(self)"));
    assert!(quarantine.contains("fn retain(self, queue: QuarantinedQueue)"));

    let prepared = without_whitespace(&prepared);
    assert_in_order(
        &prepared,
        &[
            "QueueQuarantineReservations::reserve(MAX_HARDWARE_QUEUES)",
            "materialize_logical_devices",
            "create_runtime_devices",
            "Ok(PreparedBlockController",
        ],
    );
    assert_in_order(
        &prepared,
        &[
            "validate_runtime_devices",
            "devices:self.devices.into_boxed_slice()",
            "self.owner_link.publish(&controller)",
        ],
    );

    let registry = without_whitespace(&registry);
    assert_in_order(
        &registry,
        &[
            "quarantine_reservations.bind(info)",
            "matchinfo.kind",
            "runtime_queues.push(runtime_queue)",
        ],
    );
    assert!(registry.contains("InlineQueue::new(queue,quarantine_reservation)"));
    assert!(registry.contains("HardwareQueue::activate(queue,quarantine_reservation,"));

    let device = without_whitespace(&device);
    let inline_queue = device
        .split_once("structInlineQueue")
        .expect("inline queues must own their runtime resources")
        .1
        .split_once("implBlockController")
        .expect("inline queue ownership must precede controller methods")
        .0;
    assert!(inline_queue.contains("QueueQuarantineReservation"));
    assert!(inline_queue.contains("close_or_quarantine(queue,"));
    assert!(inline_queue.contains("quarantine_live_queue(queue,reason,"));

    let hctx = without_whitespace(&hctx);
    assert!(hctx.contains("quarantine_reservation:QueueQuarantineReservation"));
    assert!(
        hctx.contains("quarantine_reservation:SpinNoPreempt<Option<QueueQuarantineReservation>>")
    );
    let lifecycle = without_whitespace(&lifecycle);
    assert!(lifecycle.contains("close_or_quarantine(driver,reservation)"));
    assert!(lifecycle.contains("quarantine_live_queue(queue,reason,reservation)"));

    let owner_activation = activation
        .split_once("fn run_controller_owner")
        .expect("controller activation must run on its final maintenance owner")
        .1
        .split_once("fn bind_normal_sources")
        .expect("owner activation must precede normal IRQ binding")
        .0;
    let owner_activation = without_whitespace(owner_activation);
    assert_in_order(
        &owner_activation,
        &[
            "retire_initial_sources(&mutinitialized)",
            "BlockController::prepare_on_owner",
            "bind_normal_sources",
            "prepared.commit_on_owner",
            "enable_runtime_sources",
            "activation.publish(Ok",
        ],
    );
}

#[test]
fn portable_controller_callbacks_run_outside_the_spin_preempt_guard() {
    let controller = read_workspace_file("os/arceos/modules/axruntime/src/block/controller/mod.rs");
    let owner_paths = [
        ("controller", controller.as_str()),
        (
            "handoff owner",
            include_str!("../src/block/controller/handoff_owner.rs"),
        ),
        (
            "shutdown owner",
            include_str!("../src/block/controller/shutdown_owner.rs"),
        ),
        (
            "recovery owner",
            include_str!("../src/block/controller/recovery.rs"),
        ),
        (
            "IRQ route owner",
            include_str!("../src/block/controller/irq_routes.rs"),
        ),
    ];

    for (scope, source) in owner_paths {
        let source = without_whitespace(source);
        for forbidden in [
            "device.lock().enable_irq(",
            "device.lock().disable_irq(",
            "device.lock().bundle_mut(",
        ] {
            assert!(
                !source.contains(forbidden),
                "{scope} invokes portable driver callback while SpinNoPreempt is held: {forbidden}"
            );
        }
    }

    let controller = without_whitespace(&controller);
    assert!(controller.contains("device:DriverEndpointSlot<RdifBlockDevice>"));
    assert!(controller.contains("endpoint:SpinNoPreempt<Option<T>>"));
    assert!(controller.contains("structDriverEndpointLease"));
    assert!(controller.contains("DropforDriverEndpointLease"));
    assert!(controller.contains("self.slot.endpoint.lock()"));
    assert!(controller.contains("self.assert_driver_endpoint_owner()"));
}
