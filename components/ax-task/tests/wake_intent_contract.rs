//! Source-level contract for synchronous wait-queue wake placement.

const WAKE: &str = include_str!("../src/system/task_system/dispatch/wake.rs");

#[test]
fn wait_claim_target_selection_preserves_the_wake_intent() {
    let delivery = function_body(WAKE, "pub(crate) fn wake_wait_claim_from_current_cpu(");
    let selection = call_arguments(delivery, "self.select_wake_target(");

    assert!(
        selection.contains("intent"),
        "a synchronous wait-queue wake must retain WF_SYNC during CPU selection"
    );
    assert!(
        !selection.contains("WakeIntent::Normal"),
        "wait-claim delivery must not silently downgrade WF_SYNC to a normal wake"
    );
}

fn function_body<'a>(source: &'a str, signature: &str) -> &'a str {
    delimited(source, signature, '{', '}')
}

fn call_arguments<'a>(source: &'a str, signature: &str) -> &'a str {
    delimited(source, signature, '(', ')')
}

fn delimited<'a>(source: &'a str, signature: &str, open: char, close: char) -> &'a str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing source item: {signature}"));
    let source = &source[start..];
    let delimiter = source
        .find(open)
        .unwrap_or_else(|| panic!("missing source body: {signature}"));
    let mut depth = 0usize;
    for (offset, character) in source[delimiter..].char_indices() {
        if character == open {
            depth += 1;
        } else if character == close {
            depth -= 1;
            if depth == 0 {
                return &source[..delimiter + offset + character.len_utf8()];
            }
        }
    }
    panic!("unterminated source item: {signature}");
}
