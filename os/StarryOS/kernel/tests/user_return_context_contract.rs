//! User traps must cross one runtime-owned task-context boundary before Starry work.

const MM_ACCESS: &str = include_str!("../src/mm/access.rs");
const USER_LOOP: &str = include_str!("../src/task/user.rs");

#[test]
fn user_return_is_accounted_and_validated_before_any_sleepable_starry_work() {
    let user_loop = function_body(USER_LOOP, "pub fn new_user_task(");
    let runtime_return = user_loop
        .find("ax_runtime::task::run_user_context(")
        .expect(
            "UserContext and task-owned CPU accounting must cross the same runtime boundary; \
             direct uctx.run() cannot prove that accounting, the scheduler baton, and preemption \
             state are finished",
        );
    let first_sleepable_work = user_loop
        .find("drain_user_return_work(")
        .expect("timer delivery must remain in an explicit ordinary task-context drain");

    assert!(
        runtime_return < first_sleepable_work,
        "the runtime must restore kernel accounting and ordinary task context before Starry \
         acquires the timer PI mutex",
    );
    assert!(
        !user_loop.contains("let reason = uctx.run();"),
        "Starry must not bypass the runtime-owned user-return boundary",
    );
}

#[test]
fn kernel_and_bootstrap_faults_fail_closed_before_user_mm_lookup() {
    let page_fault = function_body(MM_ACCESS, "fn handle_page_fault(");
    let optional_identity = page_fault
        .find("resolve_page_fault_user_task(try_current_user_task())")
        .expect("kernel fault handling must use the optional Starry user-task identity");
    let absent_identity = page_fault
        .find("Ok(None) => return false")
        .expect("kernel and bootstrap threads must fail closed without a user extension");
    let address_space = page_fault
        .find("thr.proc_data.aspace()")
        .expect("only a proven user task may reach its address space");

    assert!(optional_identity < absent_identity && absent_identity < address_space);
    assert!(
        !page_fault.contains("let curr = current_user_task()")
            && !page_fault.contains("let curr = current_user_task();"),
        "a kernel-origin fault must not use the strict user-task accessor",
    );
}

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
