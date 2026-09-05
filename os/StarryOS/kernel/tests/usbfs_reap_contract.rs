//! Source-level contract for USBFS completion readiness.

const USBFS: &str = include_str!("../src/pseudofs/usbfs/mod.rs");

#[test]
fn backend_reclaim_cannot_complete_a_blocking_reap() {
    let collect = function_source("fn collect_submitted_urbs(");
    assert!(
        !collect.contains("-> bool"),
        "reclaiming a discarded backend transfer is not a user-visible completion"
    );

    let reap = function_source("fn reap_urb(");
    assert!(
        !reap.contains("collect_submitted_urbs(None) ||")
            && !reap.contains("collect_submitted_urbs(Some(cx)) ||"),
        "blocking REAPURB may become ready only when pending_urbs contains a completion"
    );
    assert!(
        reap.contains("self.collect_submitted_urbs(None);") && reap.contains(".pop_front()"),
        "REAPURB must collect backend terminals and then check the user completion queue"
    );
    assert!(
        reap.contains("crate::task::future::block_on_user(")
            && reap.contains("crate::task::future::poll_exclusive(")
            && reap.contains("current,"),
        "blocking REAPURB must preserve Linux signal-interruptible user wait semantics"
    );
}

#[test]
fn poll_readiness_uses_the_same_user_completion_condition() {
    let register = function_source("unsafe fn register_shared(");
    assert!(
        !register.contains("collect_submitted_urbs(Some(context)) ||"),
        "a discarded backend terminal must not publish readable USBFS state"
    );
    assert!(register.contains("!self.pending_urbs.lock().is_empty()"));
}

fn function_source(signature: &str) -> &'static str {
    let start = USBFS
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let source = &USBFS[start..];
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
