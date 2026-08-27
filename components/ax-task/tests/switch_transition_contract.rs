//! Context-switch publication must leave no recoverable scheduler tail.

const PARK_EXIT: &str = include_str!("../src/system/task_system/park_exit.rs");

#[test]
fn runtime_tail_publication_has_no_recoverable_error_path() {
    let completion = function_body(
        PARK_EXIT,
        "pub(super) unsafe fn complete_context_switch_owner(",
    );
    let publication = completion
        .find("task_runtime::finish_context_switch_tail()")
        .expect("switch completion must publish the architecture runtime tail");
    let after_publication = &completion[publication..];

    assert!(
        !after_publication.contains("return Err("),
        "a published runtime tail cannot report a recoverable scheduler error"
    );
    assert!(
        !after_publication.contains("?;"),
        "all fallible validation must precede runtime-tail publication"
    );
}

#[test]
fn published_exit_commit_only_consumes_the_prepared_permit() {
    let commit = function_body(
        PARK_EXIT,
        "pub(crate) unsafe fn commit_prepared_current_exit(",
    );

    assert!(!commit.contains("Result<ScheduleDecision"));
    assert!(!commit.contains("?;"));
    assert!(!commit.contains("complete_context_switch_in_scheduler_frame"));
    assert!(!commit.contains("drain_owner_work"));
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
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[..brace + offset + character.len_utf8()];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated source item: {signature}");
}
