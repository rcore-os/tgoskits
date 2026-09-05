//! Source-level contract for Starry's executor-to-scheduler park adapter.

const FUTURE_RUNTIME: &str = include_str!("../src/task/future.rs");
const TASK_OPS: &str = include_str!("../src/task/ops.rs");

#[test]
fn executor_park_uses_the_scheduler_predicate_handshake() {
    let block_on = function_body(FUTURE_RUNTIME, "fn block_on_with_abort<F, A>(");
    let user_wait = function_body(FUTURE_RUNTIME, "async fn user_wait_future<F: IntoFuture>(");

    assert!(
        block_on.contains("executor.run(future.into_future(), |condition|"),
        "the OS adapter must receive the typed executor park condition"
    );
    assert!(
        block_on.contains("let ready = || condition.should_abort() || should_abort()")
            && block_on.contains("wait.wait_until(ready)"),
        "executor work and Starry interruption must be checked inside WaitQueue park"
    );
    assert!(
        !block_on.contains("wait.wait();"),
        "an unconditional WaitQueue park can lose an executor wake drained while Running"
    );
    assert!(
        !block_on.contains("yield_current_cpu"),
        "a sticky user interruption must complete the typed wait instead of repeatedly yielding"
    );
    assert!(user_wait.contains("UserWaitOutcome<F::Output>"));
    assert!(user_wait.contains("resolve_user_wait(future, interrupted, timed_out)"));
}

#[test]
fn exec_sibling_zap_publishes_a_sticky_future_abort() {
    let zap = function_body(TASK_OPS, "pub fn zap_thread(");

    assert!(zap.contains("thr.set_exit_request()"));
    assert!(
        zap.contains("task.interrupt()"),
        "a direct scheduler wake has no persistent LocalExecutor abort reason"
    );
    assert!(!zap.contains("task.wake_handle().wake()"));
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let source = &source[start..];
    let brace = source
        .find('{')
        .unwrap_or_else(|| panic!("missing function body: {signature}"));
    let mut depth = 0usize;
    for (offset, byte) in source[brace..].bytes().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[..brace + offset + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body: {signature}");
}
