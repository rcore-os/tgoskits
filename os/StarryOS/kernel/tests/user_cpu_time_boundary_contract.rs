//! Starry CPU-time accounting must remain separate from timer delivery.

const TIMER: &str = include_str!("../src/task/timer.rs");
const TASK_OPS: &str = include_str!("../src/task/ops.rs");
const USER_LOOP: &str = include_str!("../src/task/user.rs");

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
fn user_loop_lends_cpu_accounting_to_the_runtime_owned_boundary() {
    let user_loop = function_body(USER_LOOP, "pub fn new_user_task(");
    assert!(
        user_loop.contains("curr.user_entry_notification()"),
        "Starry must lend its task-owned accounting object to the runtime boundary instead of \
         changing execution state after IRQs are already enabled",
    );
    assert!(
        !user_loop.contains("set_timer_state("),
        "the OS task loop must not acquire an ordinary PreemptGuard to emulate an entry-boundary \
         transition",
    );
}

#[test]
fn cpu_accounting_callback_is_bounded_atomic_work_only() {
    let accounting = function_body(
        TIMER,
        "unsafe impl ax_runtime::task::UserContextAccounting for CpuTimeAccounting",
    );

    for forbidden in [
        "PreemptGuard",
        ".lock()",
        "poll(",
        "send_signal",
        "notify(",
        "schedule",
    ] {
        assert!(
            !accounting.contains(forbidden),
            "IRQ-off CPU accounting must not perform `{forbidden}`",
        );
    }
    assert!(
        accounting.contains("set_state_at("),
        "the callback should only publish the timestamped user/kernel accounting transition",
    );
}

#[test]
fn timer_poll_and_signal_delivery_remain_explicit_task_context_work() {
    let user_loop = function_body(USER_LOOP, "pub fn new_user_task(");
    let runtime_return = user_loop
        .find("ax_runtime::task::run_user_context(")
        .expect("missing runtime-owned user accounting boundary");
    let reason_dispatch = user_loop
        .find("match reason")
        .expect("missing user exception dispatch");
    let drain_call = user_loop
        .rfind("drain_user_return_work(&curr")
        .expect("timer and signal delivery must run in the versioned task-context drain");

    assert!(
        runtime_return < reason_dispatch && reason_dispatch < drain_call,
        "timer delivery may run only after the runtime restored kernel accounting and the real \
         architecture exit was dispatched",
    );

    let drain = function_body(USER_LOOP, "fn drain_user_return_work(");
    assert!(drain.contains("poll_timer(task)"));
    assert!(drain.contains("finish_user_return_work(snapshot)"));

    let poll_timer = function_body(TASK_OPS, "pub fn poll_timer(");
    assert!(poll_timer.contains("thr.time.lock().poll(&thr.cpu_time)"));
    assert!(poll_timer.contains("send_signal_thread_inner"));
}

#[test]
fn timer_delivery_is_not_hidden_inside_an_accounting_helper() {
    assert!(
        !TASK_OPS.contains("pub fn set_timer_state("),
        "the combined helper hides a sleepable timer poll and signal delivery behind a CPU \
         accounting name; callers must use the runtime transition and poll_timer separately",
    );
}
