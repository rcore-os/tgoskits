//! Source-level contract for Linux-style pipe blocking ownership.

const PIPE: &str = include_str!("../src/file/pipe.rs");
const FUTURE: &str = include_str!("../src/task/future.rs");

#[test]
fn ready_pipe_io_bypasses_the_blocking_future_path() {
    let shared = struct_body(PIPE, "struct Shared");
    let read = function_body(PIPE, "fn read(&self,");
    let write = function_body(PIPE, "fn write_with_broken_pipe_handler(");

    assert!(
        !shared.contains("WaitQueue"),
        "pipe readiness must keep one waiter-selection owner in PollSet"
    );
    for operation in [read, write] {
        let fast_path = operation
            .find("operation(ExclusivePollWake::Unselected)")
            .expect("pipe I/O must attempt the operation synchronously");
        let blocking_path = operation
            .find("block_on_user(")
            .expect("blocking pipe I/O must retain the cancellation-safe future path");
        assert!(
            fast_path < blocking_path,
            "ready pipe I/O must finish before constructing a future executor"
        );
    }
    assert!(
        FUTURE.contains("pub enum ExclusivePollWake"),
        "exclusive wake selection remains the shared PollSet slow-path contract"
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
