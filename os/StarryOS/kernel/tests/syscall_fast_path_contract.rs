//! Source-level contracts for Linux-style syscall exit fast paths.

const SIGNAL: &str = include_str!("../src/task/signal.rs");
const THREAD_SYSCALLS: &str = include_str!("../src/syscall/task/thread.rs");
const DEVICES: &str = include_str!("../src/pseudofs/dev/mod.rs");
const CLOCK_EVENT_RUNTIME: &str =
    include_str!("../../../arceos/modules/axruntime/src/clock_event_runtime.rs");
const USER_TASK: &str = include_str!("../src/task/user.rs");
const PTRACE: &str = include_str!("../src/task/process_ptrace.rs");

#[test]
fn unlimited_rttime_skips_watchdog_accounting() {
    let check = function_body(SIGNAL, "fn queue_rttime_limit_signal(");
    let unlimited = check
        .find("if soft_limit_us == u64::MAX")
        .expect("the default unlimited RLIMIT_RTTIME must have an explicit fast path");
    let watchdog = check
        .find(".rttime()")
        .expect("armed RLIMIT_RTTIME must still consult the thread watchdog");

    assert!(
        unlimited < watchdog,
        "unlimited RLIMIT_RTTIME must return before locking or sampling CPU time"
    );
}

#[test]
fn getpid_reads_the_immutable_process_binding_directly() {
    let getpid = function_body(THREAD_SYSCALLS, "pub fn sys_getpid(");

    assert!(
        getpid.contains(".active_number()"),
        "getpid must read the process identity's immutable active-namespace binding"
    );
    assert!(
        !getpid.contains("active_pid_namespace") && !getpid.contains("PidView"),
        "getpid must not lock thread PID ownership, clone a namespace, or scan PID bindings"
    );
}

#[test]
fn dev_null_is_an_always_ready_file() {
    let null = &DEVICES[DEVICES
        .find("struct Null;")
        .expect("the null device must exist")..];
    let flags = function_body(null, "fn flags(&self)");

    assert!(
        flags.contains("NodeFlags::BLOCKING"),
        "/dev/null must not construct a readiness future for an operation that cannot block"
    );
}

#[test]
fn scheduler_tick_classifies_the_saved_irq_origin() {
    let timer_irq = function_body(CLOCK_EVENT_RUNTIME, "pub(crate) fn timer_irq_handler(");
    assert!(
        timer_irq.contains("ctx.origin")
            && timer_irq.contains("IrqOrigin::User")
            && timer_irq.contains("SchedulerTickMode::User"),
        "the periodic tick must classify CPU time from the interrupted context"
    );
    assert!(
        !USER_TASK.contains("set_timer_state") && !USER_TASK.contains("TimerState"),
        "syscall boundaries must not mirror the current execution mode"
    );
}

#[test]
fn inactive_ptrace_work_does_not_resolve_the_thread_tid() {
    let user_task = function_body(USER_TASK, "pub fn new_user_task(");
    let user_loop = &user_task[user_task
        .find("while !thr.pending_exit()")
        .expect("the user execution loop must exist")..];
    let before_first_run = &user_loop[..user_loop
        .find(".enter()")
        .expect("the user execution loop must enter userspace")];
    let singlestep_gate = before_first_run
        .find("has_ptrace_singlestep_work()")
        .expect("single-step work must have a lock-free gate");
    let tid_resolution = before_first_run
        .find("thr.tid()")
        .expect("active single-step work must still resolve the exact tid");
    assert!(
        singlestep_gate < tid_resolution,
        "an ordinary userspace entry must not resolve tid before single-step work is pending"
    );

    let pending_event = function_body(USER_TASK, "fn stop_for_pending_ptrace_event(");
    let pending_gate = pending_event
        .find("has_ptrace_pending_event()")
        .expect("pending ptrace events must have a lock-free gate");
    let tid_resolution = pending_event
        .find("thr.tid()")
        .expect("pending ptrace work must still resolve the exact tid");
    assert!(
        pending_gate < tid_resolution,
        "the pending-event gate must run before tid resolution"
    );

    let syscall_gate = function_body(PTRACE, "pub(super) fn syscall_trace_if_active(");
    let inactive_return = syscall_gate
        .find("return None")
        .expect("an inactive ptrace relationship must return without slow work");
    let tid_resolution = syscall_gate
        .find("resolve_tid()")
        .expect("the ptrace syscall slow path must resolve its tid lazily");
    assert!(
        inactive_return < tid_resolution,
        "syscall ptrace work must pass its atomic gate before resolving tid"
    );
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing source item: {signature}"));
    let source = &source[start..];
    let brace = source
        .find('{')
        .unwrap_or_else(|| panic!("missing source body: {signature}"));
    let mut depth = 0usize;
    for (offset, character) in source[brace..].char_indices() {
        if character == '{' {
            depth += 1;
        } else if character == '}' {
            depth -= 1;
            if depth == 0 {
                return &source[..brace + offset + character.len_utf8()];
            }
        }
    }
    panic!("unterminated source item: {signature}");
}
