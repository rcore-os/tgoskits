//! Source-level contract for Starry's executor-to-scheduler park adapter.

const FUTURE_RUNTIME: &str = include_str!("../src/task/future.rs");
const TASK_SIGNAL: &str = include_str!("../src/task/signal.rs");

fn block_on_source() -> &'static str {
    FUTURE_RUNTIME
        .split_once("pub fn block_on<F: IntoFuture>")
        .expect("future runtime must define block_on")
        .1
        .split_once("/// Coalesced hard-IRQ notification")
        .expect("block_on must precede IRQ notification support")
        .0
}

fn timer_worker_source() -> &'static str {
    FUTURE_RUNTIME
        .split_once("fn timer_worker()")
        .expect("future runtime must define timer_worker")
        .1
        .split_once("fn publish_timer_change()")
        .expect("timer worker must precede timer publication")
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
        block_on.contains("block_on_with_abort(future, None, || false)"),
        "generic kernel workers must not inherit Starry signal state"
    );
    assert!(
        block_on.contains(
            "block_on_with_abort(future, Some(task.id()), || task.interruption_pending())"
        ),
        "user waits must carry an explicit proven user-task identity"
    );
    assert!(
        !block_on.contains("wait.wait();"),
        "an unconditional WaitQueue park can lose an executor wake drained while Running"
    );
}

#[test]
fn sigwait_wake_publishes_the_executor_abort_condition() {
    let send_signal = TASK_SIGNAL
        .split_once("pub fn send_signal_to_process(")
        .expect("task signal support must define send_signal_to_process")
        .1
        .split_once("/// Sends a signal to a process group.")
        .expect("process signal delivery must precede process-group delivery")
        .0;
    let blocked_waiter = send_signal
        .split_once("All threads have this signal blocked")
        .expect("blocked process signals must have a sigwait wake path")
        .1;

    assert!(
        blocked_waiter.contains("task.interrupt();"),
        "sigwait wake must publish the interruption predicate observed by block_on_user"
    );
    assert!(
        !blocked_waiter.contains("task.wake_handle().wake()"),
        "a direct scheduler wake does not make the LocalExecutor root coroutine ready"
    );
}

#[test]
fn timer_worker_captures_publication_epoch_before_runtime_snapshot() {
    let timer_worker = timer_worker_source();
    let epoch_load = timer_worker
        .find("TIMER_EPOCH.load(Ordering::Acquire)")
        .expect("timer worker must capture the publication epoch");
    let runtime_snapshot = timer_worker
        .find("TIMER_RUNTIME.lock()")
        .expect("timer worker must snapshot the timer runtime");

    assert!(
        epoch_load < runtime_snapshot,
        "capturing the epoch after the runtime snapshot can absorb a concurrent publication and \
         sleep forever"
    );
}
