//! CPU-time accounting mutations must follow scheduler owner commits.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

const TIMER: &str = include_str!("../src/task/timer.rs");
const SCHEDULER_TASK: &str = include_str!("../src/task/scheduler_task.rs");
const SCHEDULE_SYSCALL: &str = include_str!("../src/syscall/task/schedule.rs");
const AX_TASK_SPEC: &str = include_str!("../../../../components/ax-task/src/thread/spec.rs");
const AX_TASK_SYSTEM: &str =
    include_str!("../../../../components/ax-task/src/system/task_system.rs");

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let body_start = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("missing function body: {signature}"));

    let mut depth = 0usize;
    for (offset, character) in source[body_start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..=body_start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body: {signature}")
}

#[test]
fn remote_policy_syscall_only_publishes_the_scheduler_generation() {
    let update = function_body(SCHEDULE_SYSCALL, "fn apply_scheduler_update(");

    assert!(
        update.contains("scheduler::set_thread_policy("),
        "the syscall must publish the validated policy through ax-task",
    );
    assert!(
        !update.contains("set_accounting_policy("),
        "the calling CPU must not mutate a remote target's CPU-time accounting before the owner \
         CPU applies that policy generation",
    );
}

#[test]
fn applied_policy_generation_notifies_the_extension_at_the_owner_safe_point() {
    let extension_ops = function_body(AX_TASK_SPEC, "pub struct ThreadExtensionOps");
    assert!(
        extension_ops.contains("on_policy_applied"),
        "OS policy metadata needs a bounded callback tied to the scheduler's applied generation",
    );

    let apply = function_body(AX_TASK_SYSTEM, "fn apply_owner_policy_generation(");
    let unlock = apply
        .find("drop(sched)")
        .expect("the scheduler transaction must release its internal lock");
    let callback = apply
        .find("on_policy_applied")
        .expect("the owner commit must notify the OS extension");
    assert!(
        unlock < callback,
        "the policy callback must run after internal scheduler locks are released",
    );
    let deferred_release = apply
        .find("defer_deadline_admission_release")
        .expect("the owner commit must release obsolete admission capacity");
    assert!(
        callback < deferred_release,
        "an already committed policy generation must notify OS metadata before internal deferred \
         cleanup",
    );
    assert!(
        apply.contains("now_ns"),
        "scheduler policy and OS accounting must commit at the same owner timestamp",
    );

    let defer_release = function_body(AX_TASK_SYSTEM, "fn defer_deadline_admission_release(");
    assert!(
        defer_release.contains("fatal_invariant"),
        "post-commit admission accounting overflow is an invariant failure, not a recoverable \
         error that may skip the OS callback",
    );
    assert!(
        !defer_release.contains("Result<"),
        "post-commit cleanup cannot return an ordinary error after the scheduler state is \
         observable",
    );
}

#[test]
fn starry_policy_callback_uses_the_owner_commit_timestamp() {
    assert!(
        SCHEDULER_TASK.contains("starry_user_task_policy_applied"),
        "Starry must consume the generic owner-side policy commit callback",
    );
    let callback = function_body(
        SCHEDULER_TASK,
        "unsafe extern \"Rust\" fn starry_user_task_policy_applied(",
    );
    assert!(callback.contains("set_realtime_policy_at("));
    assert!(callback.contains("now_ns"));
    assert!(
        !callback.contains("monotonic_time_nanos("),
        "the extension must use ax-task's commit timestamp rather than sampling another clock \
         epoch",
    );
}

#[test]
fn first_switch_in_starts_in_kernel_accounting_state() {
    let switch_in = function_body(TIMER, "fn scheduler_switch_in_at(");

    assert!(
        switch_in.contains("TimerState::None") && switch_in.contains("TimerState::Kernel"),
        "a new task must transition None -> Kernel before its first user entry so bootstrap work \
         is not lost",
    );
}

#[test]
fn switch_out_rejects_a_user_accounting_state() {
    let switch_out = function_body(TIMER, "fn scheduler_switch_out_at(");

    assert!(
        switch_out.contains("TimerState::Kernel"),
        "switch-out must require that IRQ/user return already transitioned accounting to Kernel",
    );
    assert!(
        switch_out.contains("assert") || switch_out.contains("fatal"),
        "a User state at switch-out is an invariant violation, not a state to repair silently",
    );
}

#[test]
fn accounting_mutations_use_one_odd_even_sequence() {
    assert!(
        TIMER.contains("sequence: AtomicU64"),
        "readers need one sequence that covers the entire accounting transaction",
    );
    assert!(
        !TIMER.contains("completed_writes") && !TIMER.contains("writers: AtomicUsize"),
        "a writer count can become observably even while two split writers still overlap",
    );

    let begin = function_body(TIMER, "fn begin_write(");
    assert!(
        begin.contains("compare_exchange") && begin.contains("even") && begin.contains("odd"),
        "a writer must exclusively change one even generation to odd before mutation",
    );
    let snapshot = function_body(TIMER, "fn snapshot_at(");
    assert!(
        snapshot.contains("sequence & 1")
            && snapshot.contains("self.sequence.load(Ordering::Acquire) == sequence"),
        "a reader must reject an odd generation and retry unless the same even generation is \
         observed after the snapshot",
    );
}

#[test]
fn eager_remote_policy_mutation_charges_time_before_owner_commit() {
    let eager = PolicyClock::new(false);
    eager.change_policy_at(true, 10);
    eager.account_until(40);

    let owner_committed = PolicyClock::new(false);
    owner_committed.account_until(30);
    owner_committed.change_policy_at(true, 30);
    owner_committed.account_until(40);

    assert_eq!(eager.realtime_ns.load(Ordering::Relaxed), 30);
    assert_eq!(owner_committed.realtime_ns.load(Ordering::Relaxed), 10);
    assert_ne!(
        eager.realtime_ns.load(Ordering::Relaxed),
        owner_committed.realtime_ns.load(Ordering::Relaxed),
        "caller-side publication and owner-side scheduler commit are different time domains",
    );
}

#[test]
fn writer_count_does_not_serialize_a_policy_and_boundary_transaction() {
    let accounting = MixedEpochAccounting::new();

    let boundary_delta = accounting.reserve_boundary_delta(20);
    accounting.remote_policy_writer(true);
    accounting.finish_boundary_writer(boundary_delta);

    assert_eq!(accounting.writers.load(Ordering::Acquire), 0);
    assert_eq!(accounting.realtime_ns.load(Ordering::Acquire), 20);
    assert_ne!(
        accounting.realtime_ns.load(Ordering::Acquire),
        0,
        "the boundary writer observed the remote writer's new policy after reserving an older \
         time interval; a writer count hides snapshots but does not serialize transactions",
    );
}

struct PolicyClock {
    last_ns: AtomicU64,
    realtime_ns: AtomicU64,
    realtime: AtomicBool,
}

impl PolicyClock {
    fn new(realtime: bool) -> Self {
        Self {
            last_ns: AtomicU64::new(0),
            realtime_ns: AtomicU64::new(0),
            realtime: AtomicBool::new(realtime),
        }
    }

    fn account_until(&self, now_ns: u64) {
        let previous = self.last_ns.swap(now_ns, Ordering::AcqRel);
        if self.realtime.load(Ordering::Acquire) {
            self.realtime_ns
                .fetch_add(now_ns.saturating_sub(previous), Ordering::Relaxed);
        }
    }

    fn change_policy_at(&self, realtime: bool, now_ns: u64) {
        self.account_until(now_ns);
        self.realtime.store(realtime, Ordering::Release);
    }
}

struct MixedEpochAccounting {
    last_ns: AtomicU64,
    realtime_ns: AtomicU64,
    realtime: AtomicBool,
    writers: AtomicU64,
}

impl MixedEpochAccounting {
    fn new() -> Self {
        Self {
            last_ns: AtomicU64::new(0),
            realtime_ns: AtomicU64::new(0),
            realtime: AtomicBool::new(false),
            writers: AtomicU64::new(0),
        }
    }

    fn reserve_boundary_delta(&self, now_ns: u64) -> u64 {
        self.writers.fetch_add(1, Ordering::AcqRel);
        let previous = self.last_ns.fetch_max(now_ns, Ordering::AcqRel);
        now_ns.saturating_sub(previous)
    }

    fn remote_policy_writer(&self, realtime: bool) {
        self.writers.fetch_add(1, Ordering::AcqRel);
        self.realtime.store(realtime, Ordering::Release);
        self.writers.fetch_sub(1, Ordering::Release);
    }

    fn finish_boundary_writer(&self, delta_ns: u64) {
        if self.realtime.load(Ordering::Acquire) {
            self.realtime_ns.fetch_add(delta_ns, Ordering::Relaxed);
        }
        self.writers.fetch_sub(1, Ordering::Release);
    }
}
