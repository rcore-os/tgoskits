//! Source-level contract for Starry's executor-to-scheduler park adapter.

const FUTURE_RUNTIME: &str = include_str!("../src/task/future.rs");

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
        block_on.contains("wait.wait_until(|| condition.should_abort() || interrupted())"),
        "executor work and Starry interruption must be checked inside WaitQueue park"
    );
    assert!(
        !block_on.contains("wait.wait();"),
        "an unconditional WaitQueue park can lose an executor wake drained while Running"
    );
}
