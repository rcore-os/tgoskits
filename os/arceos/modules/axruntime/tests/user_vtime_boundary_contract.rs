//! Contract for scheduler-owned user/kernel virtual-time transitions.
//!
//! Linux brackets user execution with `user_enter_irqoff` /
//! `user_exit_irqoff` and performs virtual-time transitions inside that
//! IRQ-masked entry boundary. Zephyr likewise keeps execution accounting tied
//! to the scheduler-owned context switch instead of an ordinary preemption
//! guard owned by an OS task loop.

const RUNTIME_TASK: &str = include_str!("../src/task.rs");

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let body_start = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .unwrap_or_else(|| panic!("missing function body: {signature}"));

    let mut depth = 0usize;
    for (offset, character) in source[body_start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..=body_start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("unterminated function body: {signature}")
}

#[test]
fn runtime_exposes_a_narrow_irqoff_accounting_capability() {
    assert!(
        RUNTIME_TASK.contains("pub unsafe trait UserContextAccounting"),
        "the runtime needs a typed capability whose safety contract forbids allocation, locks, \
         scheduling, callbacks, and faults while raw IRQs are masked",
    );
    assert!(
        RUNTIME_TASK.contains("fn transition_irqoff("),
        "user/kernel accounting must be one explicit IRQ-off transition, not an ordinary \
         PreemptGuard acquired by the OS user loop",
    );
    assert!(
        RUNTIME_TASK.contains("pub enum UserExecutionState"),
        "the transition direction must be typed rather than represented by a boolean",
    );
}

#[test]
fn runtime_accounts_user_entry_after_masking_irqs() {
    let run = function_body(RUNTIME_TASK, "pub fn run_user_context(");
    let mask = run
        .find("disable_irqs")
        .expect("the runtime must own the raw IRQ-masked entry window");
    let enter_accounting = run
        .find("transition_irqoff(UserExecutionState::User")
        .expect("the runtime must publish user accounting before entering userspace");
    let enter_user = run
        .find("context.run_raw()")
        .expect("the runtime must invoke the raw architecture user context");

    assert!(
        mask < enter_accounting && enter_accounting < enter_user,
        "user accounting must transition after raw IRQ masking and before the final user entry",
    );
}

#[test]
fn runtime_accounts_exception_return_before_reenabling_irqs() {
    let run = function_body(RUNTIME_TASK, "pub fn run_user_context(");
    let return_from_user = run
        .find("context.run_raw()")
        .expect("the runtime must invoke the raw architecture user context");
    let exit_accounting = run
        .find("transition_irqoff(UserExecutionState::Kernel")
        .expect("the runtime must publish kernel accounting after a user exception");
    let enable_irqs = run
        .rfind("enable_irqs")
        .expect("the runtime must restore ordinary task IRQ state");

    assert!(
        return_from_user < exit_accounting && exit_accounting < enable_irqs,
        "kernel accounting must transition after the exception return and before any IRQ-open \
         task-context work",
    );
}

#[test]
fn runtime_boundary_borrows_the_accounting_owner_for_both_transitions() {
    let signature_start = RUNTIME_TASK
        .find("pub fn run_user_context(")
        .expect("missing runtime user-context boundary");
    let signature_end = RUNTIME_TASK[signature_start..]
        .find("{")
        .map(|offset| signature_start + offset)
        .expect("missing runtime user-context boundary body");
    let signature = &RUNTIME_TASK[signature_start..signature_end];

    assert!(
        signature.contains("accounting") && signature.contains("UserContextAccounting"),
        "the same borrowed accounting owner must cover user entry and exception return",
    );
}
