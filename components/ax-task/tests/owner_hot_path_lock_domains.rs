//! Owner-CPU scheduling must not serialize on cold task-system lock domains.

const TASK_SYSTEM: &str = include_str!("../src/system/task_system.rs");
const CPU_LOCAL: &str = include_str!("../src/system/cpu.rs");
const FACADE: &str = include_str!("../src/facade.rs");
const THREAD_SCHED: &str = include_str!("../src/system/thread_sched.rs");

#[test]
fn owner_hot_paths_do_not_acquire_global_registry_or_pi_locks() {
    for signature in [
        "pub fn drain_remote_wakes(",
        "pub fn drain_policy_updates(",
        "pub fn schedule(",
        "pub fn schedule_if_requested(",
        "fn service_deadline_timers(",
    ] {
        let body = function_body(TASK_SYSTEM, signature);
        for forbidden in [
            "self.state.lock()",
            "self.registry.lock()",
            "self.pi.lock()",
            "self.root_domain.lock()",
        ] {
            assert!(
                !body.contains(forbidden),
                "owner hot path `{signature}` acquires cold lock `{forbidden}`"
            );
        }
    }
}

#[test]
fn owner_hot_paths_use_stable_thread_scheduler_cells() {
    assert!(
        THREAD_SCHED.contains("struct ThreadSchedCell"),
        "thread scheduling state must have a stable non-registry owner"
    );
    assert!(
        TASK_SYSTEM.contains("sched: Arc<ThreadSchedCell>"),
        "generation registry records must retain the stable scheduler cell"
    );
}

#[test]
fn current_thread_queries_use_the_owner_cpu_core() {
    let current_handle = function_body(FACADE, "pub fn current_thread_handle(");
    for forbidden in [
        "runtime_task_system()",
        ".thread_handle(",
        "current_thread_id_from_cpu()",
    ] {
        assert!(
            !current_handle.contains(forbidden),
            "current-thread handle query uses cold lookup `{forbidden}`"
        );
    }
    assert!(current_handle.contains("current_thread_handle()"));

    let current_extension = function_body(FACADE, "pub fn current_thread_extension(");
    for forbidden in ["runtime_task_system()", ".thread_extension_lease("] {
        assert!(
            !current_extension.contains(forbidden),
            "current extension query uses cold lookup `{forbidden}`"
        );
    }
    assert!(
        CPU_LOCAL.contains("pub fn current_thread_handle(&self)"),
        "CpuLocal must clone its stable current core directly"
    );
}

#[test]
fn enqueue_publishes_runqueue_location_in_one_thread_sched_transaction() {
    let enqueue = function_body(TASK_SYSTEM, "fn enqueue_owner_thread(");
    assert_eq!(
        enqueue.matches("core.sched().lock()").count(),
        1,
        "affinity updates must not observe a gap between runqueue insertion and typed placement"
    );
    assert_in_order(
        enqueue,
        &[
            "let mut sched = core.sched().lock()",
            "self.enqueue_owner_thread_locked(",
            "&mut sched",
            "drop(sched)",
        ],
    );

    let locked_enqueue = function_body(TASK_SYSTEM, "fn enqueue_owner_thread_locked(");
    assert!(
        !locked_enqueue.contains("core.sched().lock()"),
        "the locked enqueue helper must reuse its caller's thread transaction"
    );
    assert_in_order(
        locked_enqueue,
        &[
            "fields.run_queue.prepare_enqueue(",
            "sched.mark_queued(owner)",
            "prepared.commit()",
        ],
    );
}

#[test]
fn runqueue_membership_has_one_typed_production_authority() {
    assert!(THREAD_SCHED.contains("struct ThreadPlacement"));
    assert!(THREAD_SCHED.contains("enum RunPlacement"));
    assert!(THREAD_SCHED.contains("enum ExecutionOwner"));
    for forbidden in [
        "queued_cpu: Option<CpuId>",
        "running_cpu: Option<CpuId>",
        "on_cpu: Option<CpuId>",
    ] {
        assert!(
            !THREAD_SCHED.contains(forbidden),
            "independent placement field `{forbidden}` would recreate a second truth"
        );
    }
    let prepare = function_body(
        include_str!("../src/scheduler/queue.rs"),
        "pub(crate) fn prepare_enqueue(",
    );
    let duplicate_scan = prepare
        .find("if self.contains(id)")
        .expect("debug builds must retain a structural consistency check");
    assert!(
        prepare[..duplicate_scan].contains("#[cfg(debug_assertions)]"),
        "release enqueue must trust typed placement instead of scanning every queue"
    );
}

#[test]
fn expired_timer_safe_point_delivery_is_bounded_and_delayed() {
    let drain = function_body(FACADE, "fn drain_current_expired_timers(");
    assert!(drain.contains("while drained < batch_limit"));
    assert!(drain.contains("cpu.defer_scheduler_work()"));
    assert!(drain.contains("cpu.arm_deferred_owner_deadline(continuation_ns)"));
    assert!(
        !drain.contains("request_reschedule"),
        "timer delivery backpressure is owner work, not a preemption reason"
    );
}

#[test]
fn schedule_out_decides_affinity_and_requeues_in_one_thread_transaction() {
    let schedule_out = function_body(TASK_SYSTEM, "fn schedule_out_owner_running(");
    assert_eq!(
        schedule_out.matches("core.sched().lock()").count(),
        1,
        "schedule-out must own one uninterrupted thread scheduling transaction"
    );
    assert_in_order(
        schedule_out,
        &[
            "let mut sched = core.sched().lock()",
            "let migration_requested =",
            "self.enqueue_owner_thread_locked(",
            "&mut sched",
        ],
    );

    for signature in ["pub fn schedule(", "pub fn schedule_if_requested("] {
        let body = function_body(TASK_SYSTEM, signature);
        assert!(
            !body.contains("sched().lock().migration_target"),
            "`{signature}` must not take a racy affinity snapshot before schedule-out"
        );
    }
}

#[test]
fn deadline_admission_and_desired_policy_update_share_one_sched_transaction() {
    let update = function_body(TASK_SYSTEM, "pub fn set_thread_policy(");
    assert_eq!(
        update.matches("sched_cell.lock()").count(),
        1,
        "owner application must not interleave with admission and desired-policy publication"
    );
    assert_in_order(
        update,
        &[
            "let mut sched = sched_cell.lock()",
            "deadline_reservation_for",
            "sched.desired_deadline_reservation",
            "drop(sched)",
        ],
    );
}

fn assert_in_order(source: &str, patterns: &[&str]) {
    let mut cursor = 0;
    for pattern in patterns {
        let offset = source[cursor..]
            .find(pattern)
            .unwrap_or_else(|| panic!("missing ordered pattern `{pattern}`"));
        cursor += offset + pattern.len();
    }
}

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function `{signature}`"));
    let source = &source[start..];
    let open = source
        .find('{')
        .unwrap_or_else(|| panic!("missing body for `{signature}`"));
    let mut depth = 0_usize;
    for (offset, character) in source[open..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[open..=open + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function `{signature}`")
}
