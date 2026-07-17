use std::{fs, path::PathBuf};

fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn workqueue_source() -> String {
    [
        "src/workqueue/mod.rs",
        "src/workqueue/types.rs",
        "src/workqueue/model.rs",
        "src/workqueue/runtime.rs",
        "src/workqueue/delayed.rs",
    ]
    .into_iter()
    .map(|path| fs::read_to_string(crate_root().join(path)).unwrap())
    .collect()
}

#[test]
fn workqueue_uses_one_directory_module_entry() {
    assert!(!crate_root().join("src/workqueue.rs").exists());
    assert!(crate_root().join("src/workqueue/mod.rs").is_file());
}

#[test]
fn workqueue_feature_selects_the_multitask_runtime() {
    let manifest = fs::read_to_string(crate_root().join("Cargo.toml")).unwrap();
    assert!(manifest.contains("workqueue = [\"multitask\"]"));
}

#[test]
fn runtime_adapter_uses_fixed_prepublication_worker_policy() {
    let workqueue = workqueue_source();
    let task = fs::read_to_string(crate_root().join("src/task.rs")).unwrap();

    assert!(workqueue.contains("static RUNTIME_WORKQUEUE"));
    assert!(workqueue.contains("initialize_workqueue_cpu"));
    assert!(workqueue.contains("Nice::new(-10)"));
    assert!(workqueue.contains("spawn_kernel_worker"));
    assert!(workqueue.contains("owns_worker_thread(current)"));
    assert!(workqueue.contains("validate_schedule_context"));
    assert!(workqueue.contains("worker_state.load(Ordering::Acquire) != WORKER_READY"));
    assert!(task.contains("fn spawn_kernel_worker"));
    assert!(task.contains("spawn_raw_with_options"));
}

#[test]
fn hard_irq_submission_uses_a_shutdown_lifetime_direct_wake_and_condition_park() {
    let model = fs::read_to_string(crate_root().join("src/workqueue/model.rs")).unwrap();
    let runtime = fs::read_to_string(crate_root().join("src/workqueue/runtime.rs")).unwrap();

    assert!(model.contains("work: Pin<&'static WorkItem>"));
    let submission = runtime
        .split_once("pub fn queue_work_on")
        .expect("runtime submission entry must exist")
        .1
        .split_once("/// Waits in task context")
        .expect("runtime submission must precede synchronous wait APIs")
        .0;
    let wake_handle = submission
        .find("lane.worker_wake_handle()?")
        .expect("submission must acquire the permanent direct wake first");
    let reservation = submission
        .find("queue.reserve_item()?")
        .expect("submission must reserve logical queue admission");
    assert!(wake_handle < reservation);
    assert!(submission.contains("worker_wake.wake()"));
    assert!(submission.contains("enforce_published_worker_progress"));
    assert!(!submission.contains("let _wake_result"));
    assert!(!submission.contains("notify_"));

    let worker_loop = runtime
        .split_once("fn runtime_worker_loop")
        .expect("fixed worker loop must exist")
        .1;
    assert!(worker_loop.contains("if lane.has_pending()"));
    assert!(
        worker_loop.contains("lane.worker_park.try_wait_until(|| lane.has_pending())?"),
        "the park handshake must recheck the same pending predicate"
    );
}

#[test]
fn a_failed_post_publication_wake_poisoned_the_lane_instead_of_rolling_back_the_node() {
    let workqueue = workqueue_source();

    assert!(workqueue.contains("enum PublishedWorkerWake"));
    assert!(workqueue.contains("WorkerInvariantLost"));
    assert!(workqueue.contains("worker_poisoned: AtomicBool"));
    assert!(workqueue.contains("poison_after_published_wake_failure"));
    assert!(workqueue.contains("WorkQueueError::WorkerPoisoned"));
    assert!(!workqueue.contains("let _wake_result = worker_wake.wake()"));
    assert!(!workqueue.contains("let _wake_result = wake.wake()"));
}

#[test]
fn production_worker_enforces_the_nonblocking_callback_contract() {
    let model = fs::read_to_string(crate_root().join("src/workqueue/model.rs")).unwrap();
    let runtime = fs::read_to_string(crate_root().join("src/workqueue/runtime.rs")).unwrap();

    assert!(model.contains("fn service_runtime_batch"));
    let guarded_callback = model
        .split_once("fn service_runtime_batch")
        .expect("production service entry must exist")
        .1
        .split_once("fn service_one")
        .expect("runtime callback policy must precede item state completion")
        .0;
    let guard = guarded_callback
        .find("PreemptGuard::new()")
        .expect("each production callback must disable scheduling");
    let callback = guarded_callback
        .find("work.callback")
        .expect("guarded path must invoke the work callback");
    assert!(guard < callback);
    assert!(runtime.contains("RUNTIME_WORKQUEUE.service_runtime_batch"));
    assert!(!runtime.contains("RUNTIME_WORKQUEUE.service_batch(route.cpu"));
}

#[test]
fn delayed_work_uses_the_task_timer_and_preallocated_control_work() {
    let workqueue = workqueue_source();
    assert!(workqueue.contains("pub struct DelayedWork"));
    assert!(workqueue.contains("control_work: WorkItem"));
    assert!(workqueue.contains("arm_current_runtime_timer"));
    assert!(workqueue.contains("dispatch_expired_timer"));
    assert!(!workqueue.contains("spawn_delayed"));
    assert!(!workqueue.contains("sleep("));
}

#[test]
fn delayed_command_and_generation_are_one_atomic_publication() {
    let delayed = fs::read_to_string(crate_root().join("src/workqueue/delayed.rs")).unwrap();

    assert!(delayed.contains("const DELAYED_COMMAND_GENERATION_BIT"));
    assert!(delayed.contains("desired_command.compare_exchange_weak("));
    assert!(!delayed.contains("desired_sequence: AtomicU64"));
}

#[test]
fn incoming_and_doorbell_use_one_seq_cst_lost_wake_handshake() {
    let model = fs::read_to_string(crate_root().join("src/workqueue/model.rs")).unwrap();

    assert!(model.contains("self.incoming.load(Ordering::SeqCst)"));
    assert!(model.contains("Ordering::SeqCst,\n                Ordering::SeqCst,"));
    assert!(model.contains("self.doorbell.store(true, Ordering::SeqCst)"));
    assert!(model.contains("self.doorbell.swap(false, Ordering::SeqCst)"));
    assert!(model.contains(".swap(ptr::null_mut(), Ordering::SeqCst)"));
}

#[test]
fn one_worker_pass_never_reverses_an_unbounded_incoming_list() {
    let model = fs::read_to_string(crate_root().join("src/workqueue/model.rs")).unwrap();

    assert!(!model.contains("reverse_list("));
}

#[test]
fn synchronous_delayed_cancel_rechecks_the_expiry_publication_baton() {
    let delayed = fs::read_to_string(crate_root().join("src/workqueue/delayed.rs")).unwrap();

    assert!(delayed.contains("self.wait_for_cancel_publication(delayed)?"));
    let retry = delayed
        .split_once("fn wait_for_cancel_publication")
        .expect("delayed cancel must own a publication retry loop")
        .1
        .split_once("/// Forces an armed delay")
        .expect("cancel publication retry must precede delayed flush")
        .0;
    assert!(retry.contains("delayed.publish_command(DELAYED_COMMAND_CANCEL)?"));
    assert!(delayed.contains("DELAYED_ARMED | DELAYED_PUBLISHING => {}"));
    assert!(delayed.contains("DELAYED_IDLE | DELAYED_QUEUED => return Ok(())"));
}

#[test]
fn synchronous_delayed_flush_rechecks_the_expiry_publication_baton() {
    let delayed = fs::read_to_string(crate_root().join("src/workqueue/delayed.rs")).unwrap();

    assert!(delayed.contains("self.wait_for_flush_publication(delayed)?"));
    assert!(delayed.contains("DELAYED_ARMED | DELAYED_PUBLISHING => {}"));
    assert!(delayed.contains("DELAYED_IDLE | DELAYED_QUEUED => return Ok(())"));
}

#[test]
fn delayed_modification_reserves_domain_admission_before_changing_phase() {
    let delayed = fs::read_to_string(crate_root().join("src/workqueue/delayed.rs")).unwrap();
    let modification = delayed
        .split_once("pub fn mod_delayed_work_on")
        .expect("delayed modification entry must exist")
        .1
        .split_once("/// Cancels a delayed timer")
        .expect("delayed modification must precede cancellation")
        .0;

    let admission = modification
        .find("DelayedDomainAdmission::acquire(self.get_ref())?")
        .expect("every delayed modification must share the domain drain linearization point");
    let phase = modification
        .find("delayed.prepare_arm()?")
        .expect("delayed modification must prepare its phase");
    assert!(admission < phase);
    assert!(modification.contains("admission.transfer_to_delayed_work()"));
}

#[test]
fn drain_completion_is_deferred_out_of_hard_irq_context() {
    let types = fs::read_to_string(crate_root().join("src/workqueue/types.rs")).unwrap();

    assert!(types.contains("lifecycle: AtomicUsize"));
    assert!(!types.contains("active_items: Atomic"));
    assert!(types.contains("drain_notify_work: WorkItem"));
    let release = types
        .split_once("fn release_item_reservation")
        .expect("logical queue must release one active-item reservation")
        .1
        .split_once("fn queue_drain_notification")
        .expect("reservation release must defer the drain notification")
        .0;
    assert!(release.contains("queue_drain_notification"));
    assert!(!release.contains("drain_wait.notify_all()"));
}

#[test]
fn synchronous_drain_validates_context_before_closing_domain_admission() {
    let runtime = fs::read_to_string(crate_root().join("src/workqueue/runtime.rs")).unwrap();
    let drain = runtime
        .split_once("pub fn drain_workqueue")
        .expect("synchronous drain entry must exist")
        .1
        .split_once("#[cfg(feature = \"workqueue\")]\n#[derive(Clone, Copy)]")
        .expect("synchronous drain implementation must precede runtime tokens")
        .0;

    let validation = drain
        .find("ensure_runtime_domain_wait_context()?")
        .expect("synchronous drain must validate its calling context");
    let transition = drain
        .find("self.begin_drain()?")
        .expect("synchronous drain must close domain admission");
    assert!(validation < transition);
}

#[test]
fn asynchronous_timer_failure_activates_the_delayed_callback() {
    let workqueue = workqueue_source();
    let hctx = fs::read_to_string(crate_root().join("src/block/hctx/mod.rs")).unwrap();

    assert!(workqueue.contains("publish_failure_activation"));
    assert!(hctx.contains("watchdog_work.take_failure()"));
}
