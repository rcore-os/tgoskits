//! Synchronous signal waits must wake through the future's registered waker.

const SIGNAL_SYSCALL: &str = include_str!("../src/syscall/signal.rs");
const TASK_SIGNAL: &str = include_str!("../src/task/signal.rs");
const SIGNAL_MANAGER: &str = include_str!("../../../../components/starry-signal/src/api/thread.rs");

#[test]
fn sigwait_registers_and_rechecks_its_future_waker() {
    let wait = function_body(SIGNAL_SYSCALL, "pub fn sys_rt_sigtimedwait(");

    assert!(wait.contains("signal.register_sigwait_waker(cx.waker())"));
    assert!(wait.matches("signal.dequeue_signal(&set)").count() >= 2);
    assert!(wait.contains("signal.finish_sigwait()"));
}

#[test]
fn blocked_signal_delivery_wakes_the_registered_sigwait_future() {
    let process_delivery = function_body(TASK_SIGNAL, "pub fn send_signal_to_process(");
    let thread_delivery = function_body(TASK_SIGNAL, "pub fn send_signal_to_thread(");

    assert!(SIGNAL_MANAGER.contains("struct SigwaitState"));
    assert!(SIGNAL_MANAGER.contains("waker: Option<Waker>"));
    assert!(process_delivery.contains("task.as_thread().signal.wake_sigwait(signo)"));
    assert!(thread_delivery.contains("thread.signal.wake_sigwait(signo)"));
    assert!(!process_delivery.contains("task.wake_handle().wake()"));
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
