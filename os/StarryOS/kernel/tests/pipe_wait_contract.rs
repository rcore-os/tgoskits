//! Source-level contract for Linux-style pipe blocking ownership.

const PIPE: &str = include_str!("../src/file/pipe.rs");

#[test]
fn blocking_pipe_io_uses_the_pipe_wait_queues_directly() {
    let shared = struct_body(PIPE, "struct Shared");
    let read = function_body(PIPE, "fn read(&self,");
    let write = function_body(PIPE, "fn write_with_broken_pipe_handler(");
    let sync_wake = function_body(PIPE, "fn wake_pipe_waiter_sync(");

    assert!(
        shared.contains("wait_rx: WaitQueue") && shared.contains("wait_tx: WaitQueue"),
        "blocking pipe I/O must sleep directly on Linux-style read/write wait queues"
    );
    for (operation, wait_queue) in [(read, "wait_rx.wait_until"), (write, "wait_tx.wait_until")] {
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
        sync_wake.contains("notify_one_sync()") && sync_wake.contains("poll_set.wake_with"),
        "pipe handoff must synchronously wake one blocking consumer and retained poll observers"
    );
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
