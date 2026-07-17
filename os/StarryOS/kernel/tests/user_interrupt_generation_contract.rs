//! Deterministic contract for Starry's exit-to-user notification level.
//!
//! Linux keeps `TIF_SIGPENDING` as a level, only lets the current task clear
//! it after re-evaluating the underlying signal queues, and re-reads all
//! exit-to-user work with IRQs disabled. Zephyr similarly publishes a wake,
//! ready-queue transition, and remote reschedule request as one scheduler
//! transaction. Starry cannot reuse either implementation directly because
//! its signal policy is intentionally outside `ax-task`; this contract models
//! a produced/acknowledged epoch level which preserves the same concurrency
//! invariants without executing an arbitrary callback in the IRQ-off entry
//! boundary.

const TASK: &str = include_str!("../src/task/mod.rs");
const SCHEDULER_TASK: &str = include_str!("../src/task/scheduler_task.rs");
const SIGNAL: &str = include_str!("../src/task/signal.rs");
const USER_LOOP: &str = include_str!("../src/task/user.rs");
const FUTURE: &str = include_str!("../src/task/future.rs");
const SYSCALL_SIGNAL: &str = include_str!("../src/syscall/signal.rs");
const RUNTIME_TASK: &str = include_str!("../../../arceos/modules/axruntime/src/task.rs");
const AX_TASK_HANDLE: &str = include_str!("../../../../components/ax-task/src/thread/handle.rs");
const AX_TASK_CPU: &str = include_str!("../../../../components/ax-task/src/system/cpu.rs");

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct InterruptSnapshot(u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UserReturnDecision {
    Ready,
    Retry,
}

const USER_RETURN_WORK_BUDGET: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundedPassOutcome {
    Stable,
    Yielded,
}

/// Sequential model of the intended runtime-owned epoch notification.
///
/// Production uses two atomics: producers advance `produced` with Release and
/// the owner advances `acknowledged` only through a typed snapshot. The final
/// IRQ-off entry gate uses Acquire loads and treats unequal epochs as a level.
#[derive(Debug, Default)]
struct VersionedInterruptLevel {
    produced: u64,
    acknowledged: u64,
}

impl VersionedInterruptLevel {
    fn publish(&mut self) {
        self.produced = self.produced.wrapping_add(1);
    }

    fn snapshot(&self) -> InterruptSnapshot {
        InterruptSnapshot(self.produced)
    }

    fn acknowledge(&mut self, snapshot: InterruptSnapshot) -> UserReturnDecision {
        self.acknowledged = self.acknowledged.max(snapshot.0);
        self.decision()
    }

    fn changed_since(&self, snapshot: InterruptSnapshot) -> bool {
        self.produced != snapshot.0
    }

    fn pending(&self) -> bool {
        self.produced != self.acknowledged
    }

    fn decision(&self) -> UserReturnDecision {
        if self.pending() {
            UserReturnDecision::Retry
        } else {
            UserReturnDecision::Ready
        }
    }
}

fn run_bounded_signal_pass(
    level: &mut VersionedInterruptLevel,
    queued_signals: &mut usize,
    replenish_each_step: bool,
) -> (usize, BoundedPassOutcome) {
    let snapshot = level.snapshot();
    let mut transitions = 0;
    while transitions < USER_RETURN_WORK_BUDGET && *queued_signals != 0 {
        *queued_signals -= 1;
        transitions += 1;
        if replenish_each_step {
            *queued_signals += 1;
            level.publish();
        }
    }

    if transitions == USER_RETURN_WORK_BUDGET {
        // Budget exhaustion deliberately leaves the captured epoch
        // unacknowledged. The real owner yields in kernel mode before starting
        // another pass, so a signal flood cannot monopolize one CPU.
        (transitions, BoundedPassOutcome::Yielded)
    } else {
        assert_eq!(level.acknowledge(snapshot), UserReturnDecision::Ready);
        (transitions, BoundedPassOutcome::Stable)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UserEntryOutcome {
    Running,
    ImmediateIrqReturn,
}

/// Models the final IRQ-off gate and the scheduler doorbell that closes the
/// remaining race between the last level read and architectural user entry.
#[derive(Debug, Default)]
struct UserEntryBoundary {
    interrupt: VersionedInterruptLevel,
    raw_irqs_masked: bool,
    entry_permitted: bool,
    remote_doorbell_pending: bool,
}

impl UserEntryBoundary {
    fn mask_irqs(&mut self) {
        self.raw_irqs_masked = true;
    }

    fn publish_remote(&mut self) {
        self.interrupt.publish();
        self.remote_doorbell_pending = true;
    }

    fn final_check(&mut self) -> UserReturnDecision {
        assert!(self.raw_irqs_masked);
        let decision = self.interrupt.decision();
        self.entry_permitted = decision == UserReturnDecision::Ready;
        decision
    }

    fn enter_user(&mut self) -> UserEntryOutcome {
        assert!(self.raw_irqs_masked);
        assert!(self.entry_permitted);
        self.raw_irqs_masked = false;
        if self.remote_doorbell_pending {
            UserEntryOutcome::ImmediateIrqReturn
        } else {
            UserEntryOutcome::Running
        }
    }
}

#[test]
fn producer_before_snapshot_is_acknowledged_after_work_drains() {
    let mut level = VersionedInterruptLevel::default();
    level.publish();

    let snapshot = level.snapshot();
    assert!(level.pending());
    assert_eq!(level.acknowledge(snapshot), UserReturnDecision::Ready);
    assert!(!level.pending());
}

#[test]
fn producer_after_snapshot_survives_acknowledgement() {
    let mut level = VersionedInterruptLevel::default();
    level.publish();
    let snapshot = level.snapshot();

    level.publish();

    assert_eq!(level.acknowledge(snapshot), UserReturnDecision::Retry);
    assert!(level.pending());
}

#[test]
fn a_stale_acknowledgement_cannot_clear_or_regress_a_newer_generation() {
    let mut level = VersionedInterruptLevel::default();
    level.publish();
    let first = level.snapshot();
    level.publish();
    let second = level.snapshot();

    assert_eq!(level.acknowledge(first), UserReturnDecision::Retry);
    assert_eq!(level.snapshot(), second);
    assert_eq!(level.acknowledge(second), UserReturnDecision::Ready);
    assert_eq!(level.acknowledge(first), UserReturnDecision::Ready);
    assert_eq!(level.acknowledged, second.0);
}

#[test]
fn interruptible_wait_observes_a_ticket_without_stealing_user_return_work() {
    let mut level = VersionedInterruptLevel::default();
    let baseline = level.snapshot();
    assert!(!level.changed_since(baseline));

    level.publish();
    assert!(level.changed_since(baseline));
    assert!(level.pending());

    // An interruptible wait is only an observer. The user-return owner is the
    // sole path which acknowledges work after draining signals and timers.
    let user_return_snapshot = level.snapshot();
    assert_eq!(
        level.acknowledge(user_return_snapshot),
        UserReturnDecision::Ready
    );
}

#[test]
fn bounded_owner_pass_yields_without_acknowledging_unprocessed_work() {
    let mut level = VersionedInterruptLevel::default();
    level.publish();
    let mut queued_signals = USER_RETURN_WORK_BUDGET + 1;

    assert_eq!(
        run_bounded_signal_pass(&mut level, &mut queued_signals, false),
        (USER_RETURN_WORK_BUDGET, BoundedPassOutcome::Yielded)
    );
    assert_eq!(queued_signals, 1);
    assert!(
        level.pending(),
        "the exhausted pass must not acknowledge work"
    );

    assert_eq!(
        run_bounded_signal_pass(&mut level, &mut queued_signals, false),
        (1, BoundedPassOutcome::Stable)
    );
    assert!(!level.pending());
}

#[test]
fn continuous_epoch_flood_is_bounded_and_remains_pending() {
    let mut level = VersionedInterruptLevel::default();
    level.publish();
    let mut queued_signals = 1;

    for _ in 0..3 {
        assert_eq!(
            run_bounded_signal_pass(&mut level, &mut queued_signals, true),
            (USER_RETURN_WORK_BUDGET, BoundedPassOutcome::Yielded)
        );
        assert_eq!(queued_signals, 1);
        assert!(level.pending());
    }
}

#[test]
fn coalesced_epoch_only_flood_also_reaches_the_fairness_budget() {
    let mut level = VersionedInterruptLevel::default();
    level.publish();
    let mut transitions = 0;

    while transitions < USER_RETURN_WORK_BUDGET {
        let snapshot = level.snapshot();
        // The underlying signal is already coalesced, so no signal dequeue
        // succeeds. A producer racing the pass still advances the level.
        level.publish();
        assert_eq!(level.acknowledge(snapshot), UserReturnDecision::Retry);
        transitions += 1;
    }

    assert_eq!(transitions, USER_RETURN_WORK_BUDGET);
    assert!(level.pending());
}

#[test]
fn producer_before_the_irqoff_gate_forces_a_retry() {
    let mut boundary = UserEntryBoundary::default();
    boundary.publish_remote();
    boundary.mask_irqs();

    assert_eq!(boundary.final_check(), UserReturnDecision::Retry);
    assert!(!boundary.entry_permitted);
}

#[test]
fn producer_after_the_irqoff_gate_is_retained_and_traps_back_from_user() {
    let mut boundary = UserEntryBoundary::default();
    boundary.mask_irqs();
    assert_eq!(boundary.final_check(), UserReturnDecision::Ready);

    // A remote CPU publishes after the final level read. Raw IRQ masking
    // prevents the doorbell from being consumed before architectural entry.
    boundary.publish_remote();

    assert_eq!(boundary.enter_user(), UserEntryOutcome::ImmediateIrqReturn);
    assert!(boundary.interrupt.pending());
}

#[test]
fn starry_owns_a_versioned_level_instead_of_a_clearable_boolean() {
    assert!(
        TASK.contains("user_entry_notification: UserEntryNotification"),
        "Starry Thread must own the runtime's typed epoch notification"
    );
    assert!(
        !TASK.contains("interrupted: AtomicBool"),
        "a boolean cannot distinguish a producer racing an old consumer snapshot"
    );
}

#[test]
fn producer_publishes_the_starry_level_before_waking_the_scheduler() {
    let interrupt = function_body(SCHEDULER_TASK, "pub fn interrupt(&self)");
    let publish = interrupt
        .find("user_entry_notification.publish()")
        .or_else(|| interrupt.find("publish_user_entry_work()"))
        .expect("interrupt must Release-publish the user-entry epoch");
    let wake = interrupt
        .find("wake_handle().wake()")
        .expect("interrupt must directly wake the scheduler thread");

    assert!(
        publish < wake,
        "Release publication of the reason must precede scheduler wake/IPI publication"
    );
}

#[test]
fn no_consumer_can_unconditionally_clear_a_concurrent_notification() {
    for (name, source) in [
        ("scheduler adapter", SCHEDULER_TASK),
        ("user-return loop", USER_LOOP),
        ("ptrace/signal path", SIGNAL),
    ] {
        assert!(
            !source.contains("clear_interrupt("),
            "{name} must acknowledge an observed snapshot, not clear current state"
        );
    }
    assert!(
        !SCHEDULER_TASK.contains("interrupted.swap(false")
            && !SCHEDULER_TASK.contains("interrupted.store(false"),
        "interruptible waits must observe a typed ticket without clearing the entry level"
    );
}

#[test]
fn user_return_work_is_snapshot_drained_and_generation_acknowledged() {
    let drain = function_body(USER_LOOP, "fn drain_user_return_work(");
    let begin = drain
        .find("begin_user_return_work()")
        .expect("user return must snapshot its interruption generation");
    let signal_drain = drain
        .find("process_one_signal")
        .expect("user return must process pending signals one transition at a time");
    let timer_drain = drain
        .find("poll_timer(task)")
        .expect("user return must drain timer work");
    let finish = drain
        .find("finish_user_return_work(")
        .expect("user return must acknowledge only its captured generation");

    assert!(begin < signal_drain && signal_drain < timer_drain && timer_drain < finish);
    assert!(
        drain.contains("UserReturnDecision::Retry"),
        "new work published while draining must repeat the exit-to-user work loop"
    );
    assert!(
        USER_LOOP.contains("const USER_RETURN_WORK_BUDGET: usize = 64"),
        "one exit-to-user pass must have a fixed transition budget"
    );
    let budget_exhausted = drain
        .find("if transitions == USER_RETURN_WORK_BUDGET")
        .expect("a full signal batch must take the fairness path");
    let leave_pending = drain[budget_exhausted..]
        .find("drop(snapshot)")
        .map(|offset| budget_exhausted + offset)
        .expect("the fairness path must leave its epoch unacknowledged");
    let fair_yield = drain[leave_pending..]
        .find("yield_now()")
        .map(|offset| leave_pending + offset)
        .expect("budget exhaustion must enter a safe kernel scheduler point");
    let retry = drain[fair_yield..]
        .find("continue")
        .map(|offset| fair_yield + offset)
        .expect("the owner must retry without returning to user mode");
    assert!(budget_exhausted < leave_pending && leave_pending < fair_yield && fair_yield < retry);
    assert!(
        retry < finish,
        "the exhausted pass must retry before any epoch acknowledgement"
    );
    assert!(
        !drain.contains("while check_signals"),
        "an unbounded signal drain can monopolize a CPU under continuous publication"
    );
    let retry_arm = drain
        .find("UserReturnDecision::Retry => {")
        .expect("a generation race must enter an explicit bounded retry arm");
    let retry_budget = drain[retry_arm..]
        .find("transitions += 1")
        .expect("an epoch-only flood must consume the shared fairness budget");
    let retry_yield = drain[retry_arm..]
        .find("yield_now()")
        .expect("epoch-only exhaustion must yield before retrying user return");
    assert!(retry_budget < retry_yield);

    let user_loop = function_body(USER_LOOP, "pub fn new_user_task(");
    let deferred = user_loop
        .find("RunUserContextOutcome::Deferred")
        .expect("runtime may defer entry when work is pending");
    let deferred_drain = user_loop[deferred..]
        .find("drain_user_return_work(")
        .map(|offset| deferred + offset)
        .expect("Deferred must go directly to the ordinary task-context drain");
    let exited = user_loop
        .find("RunUserContextOutcome::Exited(reason)")
        .expect("a real architecture exit must be dispatched separately");
    assert!(deferred < deferred_drain && deferred_drain < exited);
}

#[test]
fn runtime_owns_the_final_irqoff_user_work_gate() {
    assert!(
        RUNTIME_TASK.contains("struct UserEntryNotification")
            && RUNTIME_TASK.contains("struct UserEntryTicket")
            && RUNTIME_TASK.contains("produced: AtomicU64")
            && RUNTIME_TASK.contains("acknowledged: AtomicU64"),
        "runtime must own the lock-free epoch primitive checked at user entry"
    );
    assert!(
        RUNTIME_TASK.contains("enum RunUserContextOutcome")
            && RUNTIME_TASK.contains("Deferred")
            && RUNTIME_TASK.contains("Exited("),
        "a blocked user entry must return a typed retry outcome"
    );

    let run = function_body(RUNTIME_TASK, "pub fn run_user_context(");
    let mask = run
        .find("disable_irqs()")
        .expect("runtime must mask raw IRQs before the final check");
    let pending = run
        .find("notification.pending_irqoff()")
        .expect("runtime must directly Acquire-check the notification level");
    let deferred = run
        .find("RunUserContextOutcome::Deferred")
        .expect("pending work must defer entry without switching accounting to user");
    let accounting = run
        .find("UserExecutionState::User")
        .expect("user accounting must begin only for a real user entry");
    let raw_entry = run
        .find("context.run_raw()")
        .expect("runtime must own architectural user entry");

    assert!(
        mask < pending && pending < deferred && deferred < accounting && accounting < raw_entry
    );
    assert!(
        RUNTIME_TASK.contains("notification: &UserEntryNotification"),
        "the IRQ-off gate must receive the concrete runtime primitive, not invoke a closure"
    );
}

#[test]
fn exhausted_notification_epochs_fail_closed_without_wrapping() {
    let publish = function_body(RUNTIME_TASK, "pub fn publish(&self)");
    assert!(publish.contains("fetch_update("));
    assert!(publish.contains("checked_add(1)"));
    assert!(publish.contains("user-entry notification epoch exhausted"));

    let acknowledge = function_body(RUNTIME_TASK, "fn acknowledge_epoch(");
    assert!(
        acknowledge.contains("if epoch == u64::MAX")
            && acknowledge.contains("UserEntryAck::Pending"),
        "the terminal epoch must remain permanently pending and unacknowledgeable"
    );
}

#[test]
fn interruptible_waits_observe_but_never_ack_user_entry_work() {
    let poll = function_body(SCHEDULER_TASK, "pub fn poll_interrupt(");
    assert!(poll.contains("interruption_pending()"));
    assert!(!poll.contains("acknowledge") && !poll.contains("finish_user_return_work"));

    let interruptible = function_body(FUTURE, "pub async fn interruptible_for<");
    assert!(interruptible.contains("poll_interrupt(context).is_ready()"));
    assert!(!interruptible.contains("acknowledge"));

    let ptrace_wait = function_body(SIGNAL, "fn wait_ptrace_resume(");
    assert!(ptrace_wait.contains("interrupt_snapshot()"));
    assert!(ptrace_wait.contains("block_on_user_since("));
    assert!(ptrace_wait.contains("interruptible_for_since("));

    assert_eq!(
        USER_LOOP.matches("finish_user_return_work(").count(),
        1,
        "only the exit-to-user owner drain may acknowledge a captured epoch"
    );
    for (name, source) in [
        ("future waiters", FUTURE),
        ("signal and ptrace waiters", SIGNAL),
        ("signal syscalls", SYSCALL_SIGNAL),
    ] {
        assert!(
            !source.contains("finish_user_return_work(") && !source.contains(".acknowledge("),
            "{name} must observe notification state without acknowledging it"
        );
    }
}

#[test]
fn custom_signal_waits_never_ignore_a_ready_notification() {
    assert!(
        !SYSCALL_SIGNAL.contains("let _ = curr.poll_interrupt(cx)"),
        "ignoring a level notification leaves block_on_user in a permanent yield loop"
    );
    assert!(
        SYSCALL_SIGNAL.contains("else if curr.poll_interrupt(cx).is_ready()")
            && SYSCALL_SIGNAL.contains("curr.poll_interrupt(cx)"),
        "custom signal waits must terminate or re-evaluate real signal state on notification"
    );
}

#[test]
fn ax_task_publishes_wake_membership_before_the_remote_doorbell() {
    let wake = function_body(AX_TASK_HANDLE, "unsafe fn wake_from_arc_ptr(");
    let wake_level = wake
        .find("thread.publish_wake()")
        .expect("direct wake must first publish its scheduler-owned level");
    let remote_inbox = wake
        .find("cpu.publish_remote_wake")
        .expect("direct wake must publish owner-CPU inbox membership");
    assert!(wake_level < remote_inbox);

    let publish = function_body(AX_TASK_CPU, "pub(crate) fn publish_remote_wake(");
    let inbox = publish
        .find("publish_with_head_transition")
        .expect("remote wake must publish its intrusive inbox node");
    let doorbell = publish
        .find("kick_scheduler_work()")
        .expect("remote wake must kick the owner CPU");
    assert!(inbox < doorbell);
}

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
