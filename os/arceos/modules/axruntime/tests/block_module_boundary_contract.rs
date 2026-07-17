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
fn block_workers_do_not_hold_irq_off_locks() {
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
        inline_queue.contains("SpinNoPreempt<QueueHandle>"),
        "inline memory I/O needs migration exclusion, not a long IRQ-off critical section"
    );
    assert!(!inline_queue.contains("SpinNoIrq<QueueHandle>"));

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
        .split_once("fn register_irq_routes_disabled")
        .expect("queue rollback must remain separate from IRQ registration")
        .0;
    assert!(
        rollback.contains("queue.shutdown_unpublished()"),
        "unpublished teardown must use the inline queue's owned shutdown transaction"
    );
    assert!(device.contains("let mut queue = self.queue.lock()"));
    assert!(device.contains("queue.shutdown(&mut *rejected_owners)"));
    let drop_impl = device
        .split_once("impl Drop for InlineQueue")
        .expect("inline queue must return driver ownership on final drop")
        .1
        .split_once("impl BlockController")
        .expect("inline drop must precede logical-device publication")
        .0;
    assert!(
        drop_impl.contains(".get_mut()")
            && drop_impl.contains(".shutdown(")
            && !drop_impl.contains(".lock()"),
        "final drop must use exclusive ownership rather than racing a queue guard"
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
        .split_once("pub fn record_irq_event")
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
