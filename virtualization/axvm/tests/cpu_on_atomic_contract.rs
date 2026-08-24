//! Source contract for the PSCI CPU_ON acceptance path.
//!
//! The relevant failure is a scheduler deadlock inside an architecture VM-exit
//! boundary, which a host unit test cannot enter. This test is therefore an
//! intentional static lint over the target-only orchestration function: the
//! absence and ordering of blocking calls are themselves the contract.

const VCPU_RUNTIME: &str = include_str!("../src/runtime/vcpus.rs");

fn function_body<'source>(source: &'source str, signature: &str) -> &'source str {
    let start = source
        .find(signature)
        .unwrap_or_else(|| panic!("missing function signature: {signature}"));
    let body_start = source[start..]
        .find('{')
        .map(|offset| start + offset)
        .expect("function body must start with an opening brace");

    let mut depth = 0usize;
    for (offset, byte) in source.as_bytes()[body_start..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return &source[body_start..=body_start + offset];
                }
            }
            _ => {}
        }
    }
    panic!("function body must end with a closing brace");
}

#[test]
fn cpu_on_acceptance_does_not_wait_for_target_task() {
    let body = function_body(VCPU_RUNTIME, "pub(crate) fn vcpu_on(");

    for blocking_call in ["wait_until", "wait_for", ".wait(", ".join(", "yield_now"] {
        assert!(
            !body.contains(blocking_call),
            "PSCI CPU_ON runs inside the current-vCPU atomic boundary and must not call \
             {blocking_call} before returning after task acceptance"
        );
    }

    let reserve = body
        .find("runtime.reserve_vcpu_lifecycle_participant()")
        .expect("CPU_ON acceptance must reserve target lifecycle participation");
    let activate = body
        .find("prepared_task.activate()")
        .expect("CPU_ON acceptance must activate the prepared target task");
    assert!(
        reserve < activate,
        "target lifecycle participation must be visible before remote task activation"
    );
}
