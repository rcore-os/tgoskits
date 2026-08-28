//! Source-level contract for Linux-style pipe blocking ownership.

const PIPE: &str = include_str!("../src/file/pipe.rs");

#[test]
fn blocking_pipe_io_uses_one_linux_style_exclusive_wait_order() {
    let shared = struct_body(PIPE, "struct Shared");
    let wait_set = struct_body(PIPE, "struct PipeWaitSet");
    let read = function_body(PIPE, "fn read(&self,");
    let write = function_body(PIPE, "fn write_with_broken_pipe_handler(");
    let sync_wake = function_body(PIPE, "fn wake_pipe_waiter_sync(");
    let wake = function_body(PIPE, "fn wake(&self, ready:");

    assert!(
        shared.contains("wait_rx: PipeWaitSet") && shared.contains("wait_tx: PipeWaitSet"),
        "each pipe direction must own one composite Linux-style wait source"
    );
    assert!(
        !wait_set.contains("direct: WaitQueue")
            && wait_set.contains("state: Arc<SpinLock<PipeWaitState>>")
            && !wait_set.contains("shared: PollSet")
            && !wait_set.contains("exclusive: Arc<SpinLock")
            && !wait_set.contains("state: Arc<PiMutex"),
        "shared poll, direct task, and EPOLLEXCLUSIVE waiters must share one Linux-style \
         waitqueue owner without an internal WaitQueue or split membership lock"
    );
    for (operation, wait_queue) in [(read, "wait_rx.wait_until"), (write, "wait_tx.wait_until")] {
        let operation = compact(operation);
        assert!(
            operation.contains(wait_queue),
            "blocking pipe I/O must park on its endpoint wait queue"
        );
        assert!(
            operation.contains("take_interrupt()"),
            "direct pipe waits must preserve interruptible syscall semantics"
        );
        assert!(
            !operation.contains("block_on_user(") && !operation.contains("poll_io_with_wake("),
            "blocking pipe I/O must not construct a coroutine and a second wait queue"
        );
    }
    assert!(
        sync_wake.contains("waiters.wake(ready, true)")
            && wake.contains("PipeWakeSelection")
            && wake.contains("take_next_exclusive")
            && !wake.contains("self.shared")
            && !wake.contains("notify_one_sync()"),
        "one queue transaction must select all shared observers and one exclusive quota"
    );
}

#[test]
fn pipe_wait_predicates_use_one_lockless_readiness_snapshot() {
    let shared = struct_body(PIPE, "struct Shared");
    let read = compact(function_body(PIPE, "fn read(&self,"));
    let write = compact(function_body(PIPE, "fn write_with_broken_pipe_handler("));

    assert!(
        shared.contains("readiness: AtomicU64"),
        "pipe state transitions must publish one packed readiness snapshot"
    );
    assert!(
        read.contains(
            "self.shared.wait_rx.wait_until(||self.shared.readiness().read_wait_ready()||task.\
             interrupted())"
        ) && write.contains(
            "self.shared.wait_tx.wait_until(||self.shared.readiness().write_wait_ready()||task.\
             interrupted())"
        ),
        "Linux-style pipe wait predicates must observe readiness without retaking the pipe state \
         mutex"
    );
}

fn compact(source: &str) -> String {
    source
        .chars()
        .filter(|character| !character.is_whitespace())
        .collect()
}

fn struct_body<'a>(source: &'a str, signature: &str) -> &'a str {
    delimited_body(source, signature, '{', '}')
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    delimited_body(source, signature, '{', '}')
}

fn delimited_body<'a>(source: &'a str, signature: &str, open: char, close: char) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing source item: {signature}"));
    let source = &source[start..];
    let brace = source
        .find(open)
        .unwrap_or_else(|| panic!("missing source body: {signature}"));
    let mut depth = 0usize;
    for (offset, character) in source[brace..].char_indices() {
        if character == open {
            depth += 1;
        } else if character == close {
            depth -= 1;
            if depth == 0 {
                return &source[..brace + offset + character.len_utf8()];
            }
        }
    }
    panic!("unterminated source item: {signature}");
}
