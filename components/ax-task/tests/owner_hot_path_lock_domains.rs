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
        "affinity updates must not observe a gap between runqueue insertion and queued_cpu"
    );
    assert_in_order(
        enqueue,
        &[
            "let mut sched = core.sched().lock()",
            "fields.run_queue.enqueue(",
            "sched.queued_cpu = Some(owner)",
            "drop(sched)",
        ],
    );
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
