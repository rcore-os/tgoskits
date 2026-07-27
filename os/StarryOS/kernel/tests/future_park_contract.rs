//! Source-level contract for Starry's executor-to-scheduler park adapter.

const FUTURE_RUNTIME: &str = include_str!("../src/task/future.rs");
const TASK_OPS: &str = include_str!("../src/task/ops.rs");

fn block_on_source() -> &'static str {
    FUTURE_RUNTIME
        .split_once("pub fn block_on<F: IntoFuture>")
        .expect("future runtime must define block_on")
        .1
        .split_once("/// Coalesced hard-IRQ notification")
        .expect("block_on must precede IRQ notification support")
        .0
}

#[test]
fn executor_park_uses_the_scheduler_predicate_handshake() {
    let block_on = block_on_source();

    assert!(
        block_on.contains("executor.run(future.into_future(), |condition|"),
        "the OS adapter must receive the typed executor park condition"
    );
    assert!(
        block_on.contains("wait.wait_until(|| condition.should_abort() || should_abort())"),
        "executor work and Starry interruption must be checked inside WaitQueue park"
    );
    assert!(
        !block_on.contains("wait.wait();"),
        "an unconditional WaitQueue park can lose an executor wake drained while Running"
    );
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
